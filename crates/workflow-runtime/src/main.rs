//! `workflow run <program.toml> [--run-id <id>] [--mock <responses.jsonl>]`
//! `workflow check <program.toml>` — lint without spending a token (exit 1 on findings).
//! `workflow cost <run-dir>` — token ledger from the transcript.
//! `workflow unpark <run-dir> <step>` — clear a parked step so a rerun retries it.
//! `workflow watch <run-dir>` — live dashboard over status.json, token-free.
//! Exit codes (WORKFLOW_RUNTIME_DESIGN.md section 3, Design body):
//! 0 done · 10 escalated · 20 parked-and-blocked · 2 error · 1 check findings.
//! Without --mock, the live proxy transport is used (D4).

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
    let mut i = 0;
    match args.first().map(String::as_str) {
        Some("run") => {}
        Some("unpark") => {
            let [_, run_dir, step] = args.as_slice() else {
                return Err("usage: workflow unpark <run-dir> <step>".into());
            };
            workflow_runtime::runner::unpark(std::path::Path::new(run_dir), step)?;
            println!("unparked {step:?} — rerun the program to retry it (a rerun is a new sample)");
            return Ok(ExitCode::SUCCESS);
        }
        Some("check") => {
            let [_, program] = args.as_slice() else {
                return Err("usage: workflow check <program.toml>".into());
            };
            let repo_root = std::env::current_dir().map_err(|e| e.to_string())?;
            let findings = workflow_runtime::check::check(std::path::Path::new(program), &repo_root);
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
            return Err("usage: workflow run <program.toml> [--run-id <id>] [--mock <responses.jsonl>] | workflow check <program.toml> | workflow cost <run-dir> | workflow unpark <run-dir> <step> | workflow watch <run-dir>".into());
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
/// it's safe alongside a live run. Ctrl-C exits; nothing to clean up.
fn watch(run_dir: &std::path::Path) {
    use std::io::Write;
    loop {
        // One write per tick, cursor homed and each line erased as it's
        // overwritten. Clearing the whole screen first is what made the
        // dashboard flash once a second.
        let mut out = String::from("\x1b[H");
        for line in watch_frame(run_dir).lines() {
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

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const RED: &str = "\x1b[31m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";
const OFF: &str = "\x1b[0m";

/// Colour is off when stdout isn't a terminal or `NO_COLOR` is set; the codes
/// then collapse to empty strings so the frame stays plain text.
fn colours_on() -> bool {
    use std::io::IsTerminal;
    std::env::var_os("NO_COLOR").is_none() && std::io::stdout().is_terminal()
}

/// `84120` → `84,120`. Token counts are the numbers Peter reads mid-run.
fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn duration(secs: u64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

fn bar(done: u64, total: u64, width: usize) -> String {
    let filled = if total == 0 { 0 } else { (done.min(total) * width as u64 / total) as usize };
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// One dashboard frame, built whole so `watch` can emit it in a single write.
fn watch_frame(run_dir: &std::path::Path) -> String {
    use std::fmt::Write;
    let c = colours_on();
    let p = |code: &'static str| if c { code } else { "" };
    let (dim, bold, red, green, yellow, cyan, off) =
        (p(DIM), p(BOLD), p(RED), p(GREEN), p(YELLOW), p(CYAN), p(OFF));
    let label = |text: &str| format!("{dim}{text:<7}{off}");

    let mut f = String::new();
    let name = run_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    match workflow_runtime::runner::holder_alive(run_dir) {
        Some(pid) => _ = writeln!(f, "{bold}{name}{off}  {green}● running{off} {dim}pid {pid}{off}"),
        None => _ = writeln!(f, "{bold}{name}{off}  {dim}○ runner exited{off}"),
    }
    _ = writeln!(f, "{dim}{}{off}", "─".repeat(64));

    match workflow_runtime::status::read(run_dir) {
        Some(st) => {
            let state_colour = match st.state.as_str() {
                "run-done" => green,
                "retrying" | "waiting-on-model" | "gate" => yellow,
                "transport-error" | "parked" | "escalated" | "blocked" => red,
                _ => cyan,
            };
            if st.total_steps > 0 {
                let steps = bar(st.step_index.saturating_sub(1) as u64, st.total_steps as u64, 12);
                _ = writeln!(
                    f,
                    "{}{bold}{}{off} {dim}({}){off}  {dim}{}/{}{off} {dim}{steps}{off}",
                    label("step"),
                    st.step,
                    st.opcode,
                    st.step_index,
                    st.total_steps
                );
            }
            _ = writeln!(f, "{}{state_colour}{}{off} {}", label("state"), st.state, st.detail);
            if !st.model.is_empty() || st.max_attempts > 0 {
                let attempt = if st.attempt > 1 { yellow } else { dim };
                _ = writeln!(
                    f,
                    "{}{}  {attempt}attempt {}/{}{off}",
                    label("model"),
                    st.model,
                    st.attempt,
                    st.max_attempts
                );
            }
            let pct =
                if st.token_budget == 0 { 0 } else { st.tokens_spent * 100 / st.token_budget };
            let token_colour = if pct >= 90 {
                red
            } else if pct >= 70 {
                yellow
            } else {
                green
            };
            _ = writeln!(
                f,
                "{}{token_colour}{}{off}{dim} / {}  {} {pct}%{off}",
                label("tokens"),
                commas(st.tokens_spent),
                commas(st.token_budget),
                bar(st.tokens_spent, st.token_budget, 16)
            );
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let elapsed = now.saturating_sub(st.ts);
            // A long silence is the symptom of a stalled transport, so it goes red.
            let idle = if elapsed >= 300 {
                red
            } else if elapsed >= 120 {
                yellow
            } else {
                dim
            };
            _ = writeln!(f, "{}{idle}{} since last transition{off}", label("idle"), duration(elapsed));
            if !st.last_error.is_empty() {
                _ = writeln!(f, "\n{red}{bold}LAST ERROR{off} {red}{}{off}", st.last_error);
            }
        }
        None => _ = writeln!(f, "{dim}no status.json yet{off}"),
    }
    if let Ok(text) = std::fs::read_to_string(run_dir.join("parked.jsonl")) {
        let mut first = true;
        for line in text.lines() {
            if let Ok(p) = serde_json::from_str::<workflow_runtime::runner::ParkedItem>(line) {
                if first {
                    f.push('\n');
                    first = false;
                }
                let reason: String = p.reason.chars().take(120).collect();
                _ = writeln!(f, "{red}PARKED{off} {bold}{}{off} {}", p.step, reason);
            }
        }
    }
    f.push('\n');
    if let Ok(summary) = workflow_runtime::cost::summarize(run_dir) {
        for line in summary.lines() {
            if line.starts_with("step ") || line.starts_with("model ") {
                _ = writeln!(f, "{dim}{line}{off}");
            } else if let Some(total) = line.strip_prefix("TOTAL ") {
                _ = writeln!(f, "{bold}TOTAL{off} {total}");
            } else {
                _ = writeln!(f, "{line}");
            }
        }
    }
    f
}
