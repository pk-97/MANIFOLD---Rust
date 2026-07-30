//! `workflow watch <run-dir>` dashboard (D14): a read-only reader over
//! `status.json` plus the run dir's other on-disk state. Never touches run
//! state — safe to run alongside a live run, and it is why `frame` lives in
//! the library instead of `main.rs`: it needs to be callable from tests
//! without spawning the binary.

use std::path::Path;

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

use crate::status::commas;

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
pub fn frame(run_dir: &Path) -> String {
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
    let st = crate::status::read(run_dir);
    let elapsed = st.as_ref().map(|s| {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(s.ts)
    });
    let liveness = match crate::runner::holder_alive(run_dir) {
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
            // A running gate names its command and how long it's been going —
            // the one thing that tells a long gate apart from a hung one.
            let detail = if st.state == "gate" && st.gate_total > 0 {
                let since = elapsed.map(duration).unwrap_or_default();
                format!(
                    " {dim}·{off} gate {}/{} {dim}·{off} {} {dim}·{off} {}",
                    st.gate_index, st.gate_total, st.detail, since
                )
            } else if st.detail.is_empty() {
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
            // A lane bills wall-clock time the runner can't shorten by
            // watching — but a DEAD worker pid while the step still reads
            // waiting-on-lane means the runner is blocked on a wait() that
            // will never return. That's the wedge `watch` exists to catch.
            if st.state == "waiting-on-lane" {
                match crate::runner::lane_liveness(run_dir) {
                    Some(ll) if ll.alive => {
                        let since = ll
                            .started_at
                            .map(|t| {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                duration(now.saturating_sub(t))
                            })
                            .unwrap_or_default();
                        _ = writeln!(f, "{}{green}● running{off} {dim}pid {} {since}{off}", label("lane"), ll.pid);
                    }
                    Some(ll) => {
                        _ = writeln!(
                            f,
                            "{}{red}{bold}✗ ALARM{off} {red}pid {} is dead — the runner is wedged on a wait() that will never return{off}",
                            label("lane"),
                            ll.pid
                        );
                    }
                    None => {
                        _ = writeln!(f, "{}{dim}no lane-job.json yet{off}", label("lane"));
                    }
                }
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
            if let Ok(p) = serde_json::from_str::<crate::runner::ParkedItem>(line) {
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
    if let Ok(l) = crate::cost::ledger(run_dir) {
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
    let trail = crate::ledger::read(run_dir);
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
