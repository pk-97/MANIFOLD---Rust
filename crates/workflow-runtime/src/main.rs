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

use workflow_runtime::status::commas;

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

/// Terminal columns, so a long error wraps instead of running off the right
/// edge. 100 when stdout isn't a terminal (piped output, tests).
fn term_width() -> usize {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: TIOCGWINSZ writes a winsize through the pointer; fd 1 may be any
    // kind of file, in which case the call fails and we keep the fallback.
    let ok = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut ws) } == 0;
    if ok && ws.ws_col >= 40 { ws.ws_col as usize } else { 100 }
}

/// Wrap `text` at `width`, prefixing every line with `indent`. Model errors
/// quote file contents, so they arrive both long and full of escaped newlines.
fn wrap(text: &str, width: usize, indent: &str) -> Vec<String> {
    let body = width.saturating_sub(indent.len()).max(20);
    let mut out = Vec::new();
    for para in text.replace("\\n", " ⏎ ").lines() {
        let mut line = String::new();
        for word in para.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > body {
                out.push(format!("{indent}{line}"));
                line = String::new();
            }
            if !line.is_empty() {
                line.push(' ');
            }
            // A single word longer than the body (a pasted path or blob) is cut.
            line.extend(word.chars().take(body));
        }
        out.push(format!("{indent}{line}"));
    }
    out
}

/// One dashboard frame, built whole so `watch` can emit it in a single write.
fn watch_frame(run_dir: &std::path::Path) -> String {
    use std::fmt::Write;
    let c = colours_on();
    let p = |code: &'static str| if c { code } else { "" };
    let (dim, bold, red, green, yellow, cyan, off) =
        (p(DIM), p(BOLD), p(RED), p(GREEN), p(YELLOW), p(CYAN), p(OFF));
    let label = |text: &str| format!("{dim}{text:<7}{off}");

    let w = term_width().min(110);
    let mut f = String::new();

    // ── header: what run, is it alive, how long since it moved ──
    let name = run_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let st = workflow_runtime::status::read(run_dir);
    let elapsed = st.as_ref().map(|s| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(s.ts)
    });
    let liveness = match workflow_runtime::runner::holder_alive(run_dir) {
        Some(pid) => format!("{green}● running{off} {dim}pid {pid}{off}"),
        None => format!("{dim}○ runner exited{off}"),
    };
    // A long silence is how a stalled transport looks from outside.
    let idle = match elapsed {
        Some(s) => {
            let c = if s >= 300 {
                red
            } else if s >= 120 {
                yellow
            } else {
                dim
            };
            format!("  {dim}idle{off} {c}{}{off}", duration(s))
        }
        None => String::new(),
    };
    _ = writeln!(f, "{bold}{name}{off}  {liveness}{idle}");
    _ = writeln!(f, "{dim}{}{off}", "─".repeat(w));

    // ── progress: step, state, model, budget ──
    match &st {
        Some(st) => {
            if st.total_steps > 0 {
                _ = writeln!(
                    f,
                    "{}{dim}{}/{}{off} {} {bold}{}{off} {dim}({}){off}",
                    label("step"),
                    st.step_index,
                    st.total_steps,
                    bar(st.step_index.saturating_sub(1) as u64, st.total_steps as u64, 12),
                    st.step,
                    st.opcode
                );
            }
            if !st.title.is_empty() {
                _ = writeln!(f, "{}{dim}{}{off}", label(""), st.title);
            }
            let state_colour = match st.state.as_str() {
                "run-done" => green,
                "retrying" | "waiting-on-model" | "gate" => yellow,
                "transport-error" | "parked" | "escalated" | "blocked" => red,
                _ => cyan,
            };
            let detail = if st.detail.is_empty() {
                String::new()
            } else {
                format!(" {dim}·{off} {}", st.detail)
            };
            _ = writeln!(f, "{}{state_colour}{}{off}{detail}", label("state"), st.state);
            if !st.model.is_empty() || st.max_attempts > 0 {
                // Last attempt is the one worth catching before the step parks.
                let last = st.attempt >= st.max_attempts && st.max_attempts > 0;
                let ac = if last {
                    red
                } else if st.attempt > 1 {
                    yellow
                } else {
                    dim
                };
                let flag = if last { "  ← last try" } else { "" };
                _ = writeln!(
                    f,
                    "{}{}  {ac}attempt {}/{}{flag}{off}",
                    label("model"),
                    st.model,
                    st.attempt,
                    st.max_attempts
                );
            }
            let pct =
                if st.token_budget == 0 { 0 } else { st.tokens_spent * 100 / st.token_budget };
            let tc = if pct >= 90 {
                red
            } else if pct >= 70 {
                yellow
            } else {
                green
            };
            _ = writeln!(
                f,
                "{}{dim}{} {tc}{:>3}%{off}  {}{dim} of {}{off}",
                label("budget"),
                bar(st.tokens_spent, st.token_budget, 12),
                pct,
                commas(st.tokens_spent),
                commas(st.token_budget)
            );
            // Lanes bill dollars. Its own bar, because the token bar can sit
            // green while the expensive worker is the thing running away.
            if st.usd_budget > 0.0 {
                let upct = (st.usd_spent * 100.0 / st.usd_budget) as u64;
                let uc = if upct >= 90 {
                    red
                } else if upct >= 70 {
                    yellow
                } else {
                    green
                };
                _ = writeln!(
                    f,
                    "{}{dim}{} {uc}{upct:>3}%{off}  ${:.2}{dim} of ${:.2}{off}",
                    label("lane $"),
                    bar((st.usd_spent * 100.0) as u64, (st.usd_budget * 100.0) as u64, 12),
                    st.usd_spent,
                    st.usd_budget
                );
            }
            if !st.last_error.is_empty() {
                _ = writeln!(f, "\n{red}{bold}last error{off}");
                for line in wrap(&st.last_error, w, "  ") {
                    _ = writeln!(f, "{red}{line}{off}");
                }
            }
        }
        None => _ = writeln!(f, "{dim}no status.json yet{off}"),
    }

    // ── parked steps: the run's open questions ──
    if let Ok(text) = std::fs::read_to_string(run_dir.join("parked.jsonl")) {
        let mut first = true;
        for line in text.lines() {
            if let Ok(p) = serde_json::from_str::<workflow_runtime::runner::ParkedItem>(line) {
                if first {
                    _ = writeln!(f, "\n{red}{bold}parked{off}");
                    first = false;
                }
                _ = writeln!(f, "  {bold}{}{off} {dim}after {} attempts{off}", p.step, p.attempts);
                if let Some(t) = &p.title {
                    _ = writeln!(f, "    {dim}{t}{off}");
                }
                for line in wrap(&p.reason, w, "    ") {
                    _ = writeln!(f, "{dim}{line}{off}");
                }
            }
        }
    }

    // ── cost: where the tokens went ──
    if let Ok(l) = workflow_runtime::cost::ledger(run_dir) {
        _ = writeln!(f, "\n{dim}{:<30}{:>9}{:>12}{off}", "step", "requests", "tokens");
        for (step, s) in &l.by_step {
            _ = writeln!(f, "{step:<30}{:>9}{:>12}", s.requests, commas(s.tokens));
        }
        for (model, tokens) in &l.by_model {
            _ = writeln!(f, "{dim}{model:<39}{:>12}{off}", commas(*tokens));
        }
        _ = writeln!(f, "{bold}{:<39}{:>12}{off}", "TOTAL", commas(l.total));
        if l.usd_total > 0.0 {
            _ = writeln!(f, "{bold}{:<39}{:>12}{off}", "LANE SPEND", format!("${:.4}", l.usd_total));
        }
    }

    // ── decision trail: why the run stopped and started again ──
    let trail = workflow_runtime::ledger::read(run_dir);
    if !trail.is_empty() {
        _ = writeln!(f, "\n{dim}decisions{off}");
        for e in trail.iter().rev().take(5).rev() {
            let step = e.step.as_deref().map(|s| format!(" {s}")).unwrap_or_default();
            _ = writeln!(f, "  {bold}{}{off}{dim}{step}{off}", e.kind);
            // The note is the human's reasoning — the point of the trail.
            for text in e.note.iter().chain(e.detail.iter().filter(|_| e.note.is_none())) {
                for line in wrap(&text.chars().take(200).collect::<String>(), w, "    ") {
                    _ = writeln!(f, "{dim}{line}{off}");
                }
            }
        }
    }
    f
}
