//! `workflow run <program.toml> [--run-id <id>] [--mock <responses.jsonl>] [--reopen]`
//! `workflow check <program.toml>` — lint without spending a token (exit 1 on findings).
//! `workflow cost <run-dir>` — token and lane-dollar ledger from the transcript.
//! `workflow unpark <run-dir> <step> --note <text>` — clear a parked step so a
//! rerun retries it; the note is what you decided that makes the retry worth
//! running, and it seeds the next attempt.
//! `workflow abandon <run-dir> --reason <text>` — a run a human took over ends
//! resumed or abandoned, never neither; `run --reopen` lifts it.
//! `workflow watch <run-dir>` — live dashboard over status.json, token-free.
//! Exit codes (WORKFLOW_RUNTIME_DESIGN.md section 3, Design body):
//! 0 done · 10 escalated · 20 parked-and-blocked · 2 error · 1 check findings.
//! Without --mock, the live proxy transport is used (D4).
//!
//! `ledger.jsonl` is the decision trail: the run dir recorded every stop but
//! never the thinking, so a resumed run started from a stale status line
//! instead of the decision that unblocked it.

use std::path::PathBuf;
use std::process::ExitCode;

use workflow_runtime::runner::{Outcome, RunConfig, run};
use workflow_runtime::transport::MockTransport;

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("workflow: {msg}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut program_path: Option<PathBuf> = None;
    let mut run_id: Option<String> = None;
    let mut mock: Option<PathBuf> = None;
    let mut reopen = false;
    let mut i = 0;
    match args.first().map(String::as_str) {
        Some("run") => {}
        Some("unpark") => {
            let [_, run_dir, step, flag, note] = args.as_slice() else {
                return Err("usage: workflow unpark <run-dir> <step> --note <text>".into());
            };
            if flag != "--note" {
                return Err("usage: workflow unpark <run-dir> <step> --note <text>".into());
            }
            workflow_runtime::runner::unpark(std::path::Path::new(run_dir), step, note)?;
            println!(
                "unparked {step:?} — rerun the program to retry it; the park reason and your note seed the first attempt (a rerun is a new sample)"
            );
            return Ok(ExitCode::SUCCESS);
        }
        Some("abandon") => {
            let [_, run_dir, flag, reason] = args.as_slice() else {
                return Err("usage: workflow abandon <run-dir> --reason <text>".into());
            };
            if flag != "--reason" {
                return Err("usage: workflow abandon <run-dir> --reason <text>".into());
            }
            workflow_runtime::runner::abandon(std::path::Path::new(run_dir), reason)?;
            println!("abandoned — `workflow run ... --reopen` is the only way back");
            return Ok(ExitCode::SUCCESS);
        }
        Some("check") => {
            let [_, program] = args.as_slice() else {
                return Err("usage: workflow check <program.toml>".into());
            };
            let repo_root = std::env::current_dir().map_err(|e| e.to_string())?;
            let findings = workflow_runtime::check::check(std::path::Path::new(program), &repo_root);
            // Advisory only — printed, never counted toward exit 1.
            for w in workflow_runtime::check::warnings(std::path::Path::new(program)) {
                println!("WARNING: {w}");
            }
            if findings.is_empty() {
                println!("check green — {program} is runnable from {}", repo_root.display());
                return Ok(ExitCode::SUCCESS);
            }
            for f in &findings {
                println!("FINDING: {f}");
            }
            println!("{} finding(s)", findings.len());
            return Ok(ExitCode::from(1));
        }
        Some("cost") => {
            let [_, run_dir] = args.as_slice() else {
                return Err("usage: workflow cost <run-dir>".into());
            };
            print!("{}", workflow_runtime::cost::summarize(std::path::Path::new(run_dir))?);
            return Ok(ExitCode::SUCCESS);
        }
        Some("watch") => {
            let [_, run_dir] = args.as_slice() else {
                return Err("usage: workflow watch <run-dir>".into());
            };
            watch(std::path::Path::new(run_dir));
            return Ok(ExitCode::SUCCESS);
        }
        _ => {
            return Err("usage: workflow run <program.toml> [--run-id <id>] [--mock <responses.jsonl>] [--reopen] | workflow check <program.toml> | workflow cost <run-dir> | workflow unpark <run-dir> <step> --note <text> | workflow abandon <run-dir> --reason <text> | workflow watch <run-dir>".into());
        }
    }
    i += 1;
    while i < args.len() {
        match args[i].as_str() {
            "--run-id" => {
                i += 1;
                run_id = Some(args.get(i).ok_or("--run-id needs a value")?.clone());
            }
            "--mock" => {
                i += 1;
                mock = Some(PathBuf::from(args.get(i).ok_or("--mock needs a value")?));
            }
            "--reopen" => reopen = true,
            other if program_path.is_none() => program_path = Some(PathBuf::from(other)),
            other => return Err(format!("unexpected argument {other:?}")),
        }
        i += 1;
    }
    let program_path = program_path.ok_or("no program file given")?;

    let transport: Box<dyn workflow_runtime::transport::ModelTransport> = match mock {
        Some(mock_path) => {
            let responses: Vec<String> = std::fs::read_to_string(&mock_path)
                .map_err(|e| format!("cannot read mock file: {e}"))?
                .lines()
                .map(|l| serde_json::from_str::<String>(l).map_err(|e| format!("mock line is not a JSON string: {e}")))
                .collect::<Result<_, _>>()?;
            Box::new(MockTransport::new(responses))
        }
        None => Box::new(workflow_runtime::transport::LiveTransport::new().map_err(|e| e.to_string())?),
    };

    let repo_root = std::env::current_dir().map_err(|e| e.to_string())?;
    // Run-id defaults to the program's NAME field (the guide's contract),
    // never the file stem (finding 10: stem collisions cross-keyed run state).
    let program = workflow_runtime::program::Program::load(&program_path)?;
    let run_dir = repo_root
        .join(".claude/orchestration/runs")
        .join(run_id.unwrap_or(program.name));

    // Reopening is a decision, so it goes on the trail before anything runs.
    if reopen && run_dir.join("abandoned.json").exists() {
        workflow_runtime::runner::reopen(&run_dir)?;
        println!("reopened {} — the abandonment is lifted and on the ledger", run_dir.display());
    }

    let cfg = RunConfig { program_path, run_dir: run_dir.clone(), repo_root };
    match run(&cfg, transport.as_ref())? {
        Outcome::Done => {
            println!("done — artifacts in {}", run_dir.display());
            Ok(ExitCode::SUCCESS)
        }
        Outcome::Escalated(path) => {
            println!("ESCALATED — answer {} and rerun", path.display());
            Ok(ExitCode::from(10))
        }
        Outcome::Blocked(reason) => {
            println!("BLOCKED — {reason} (see parked.jsonl)");
            Ok(ExitCode::from(20))
        }
    }
}

/// Read-only dashboard over `status.json` (D14) — never touches run state, so
/// it's safe alongside a live run. Ctrl-C exits; nothing to clean up. The
/// frame itself is built by `workflow_runtime::watch::frame` so it's testable
/// without spawning this binary.
fn watch(run_dir: &std::path::Path) {
    use std::io::Write;
    loop {
        // One write per tick, cursor homed and each line erased as it's
        // overwritten. Clearing the whole screen first is what made the
        // dashboard flash once a second.
        let mut out = String::from("\x1b[H");
        for line in workflow_runtime::watch::frame(run_dir).lines() {
            out.push_str(line);
            out.push_str("\x1b[K\r\n");
        }
        out.push_str("\x1b[J");
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
        drop(stdout);
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
