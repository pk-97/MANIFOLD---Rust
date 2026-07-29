//! The loop (WORKFLOW_RUNTIME_DESIGN.md section 3, Design body): per step — assemble
//! context, call the model, parse the typed artifact, run the postcondition,
//! persist, advance. Failures feed back verbatim up to the retry cap, then PARK.
//! State is files; a rerun skips completed steps (D6).

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::artifacts::{Artifact, ArtifactKind, ChangeSet};
use crate::gates::run_gates;
use crate::program::{Opcode, Program, Step, Target};
use crate::template;
use crate::transport::{CompletionRequest, ModelTransport};
use crate::worktree::{self, Worktree};

pub struct RunConfig {
    pub program_path: PathBuf,
    pub run_dir: PathBuf,
    /// Gate commands and `file:` inputs resolve relative to this.
    pub repo_root: PathBuf,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// All steps completed (parked steps, if any, are in parked.json).
    Done,
    /// A question awaits an answer in the named file. Exit 10.
    Escalated(PathBuf),
    /// A step parked and a later step depends on it. Exit 20.
    Blocked(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ParkedItem {
    pub step: String,
    pub reason: String,
    pub attempts: u32,
}

const ANSWER_MARKER: &str = "## ANSWER (write below this line, then rerun)";

/// One invocation per run dir (finding 8): a double-start would double spend
/// and interleave state. Stale locks (dead pid) are reclaimed.
struct RunLock {
    path: PathBuf,
}

impl RunLock {
    fn take(run_dir: &Path) -> Result<RunLock, String> {
        let path = run_dir.join("run.lock");
        if let Ok(old) = fs::read_to_string(&path) {
            let pid: i32 = old.trim().parse().unwrap_or(0);
            // Signal 0 = existence probe. A live holder is a loud stop.
            if pid > 0 && unsafe { libc_kill_probe(pid) } {
                return Err(format!(
                    "run dir is locked by live pid {pid} ({}) — a second concurrent invocation would double spend; wait or kill it",
                    path.display()
                ));
            }
        }
        fs::write(&path, std::process::id().to_string()).map_err(|e| e.to_string())?;
        Ok(RunLock { path })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// True iff the pid exists (kill(pid, 0) succeeds or fails with EPERM).
unsafe fn libc_kill_probe(pid: i32) -> bool {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    unsafe { kill(pid, 0) == 0 }
}

/// Resume is keyed by step index+name against the LIVE program file; an
/// inserted/renamed/reordered step silently corrupts resume (finding 7).
/// The guard compares the STEP LIST, not bytes — raising `token_budget` or
/// tweaking a template between reruns are sanctioned resume flows.
fn check_program_unchanged(cfg: &RunConfig, current: &Program) -> Result<(), String> {
    let copy_path = cfg.run_dir.join("program.toml");
    if copy_path.exists() {
        let saved = Program::load(&copy_path)?;
        let key = |p: &Program| p.steps.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
        if key(&saved) != key(current) {
            return Err(format!(
                "the program's step list changed since this run started ({:?} -> {:?}) — resume would mis-key step state; use a fresh run-id",
                key(&saved),
                key(current)
            ));
        }
    }
    // Refresh the snapshot so the run dir always shows what actually ran.
    fs::copy(&cfg.program_path, &copy_path).map_err(|e| format!("cannot copy program: {e}"))?;
    Ok(())
}

pub fn run(cfg: &RunConfig, transport: &dyn ModelTransport) -> Result<Outcome, String> {
    let program = Program::load(&cfg.program_path)?;
    fs::create_dir_all(&cfg.run_dir).map_err(|e| format!("cannot create run dir: {e}"))?;
    let _lock = RunLock::take(&cfg.run_dir)?;
    check_program_unchanged(cfg, &program)?;
    let template_root = cfg
        .program_path
        .parent()
        .ok_or("program path has no parent")?
        .to_path_buf();

    let mut parked: Vec<String> = load_parked(&cfg.run_dir)?.into_iter().map(|p| p.step).collect();
    let mut artifacts: BTreeMap<String, Artifact> = BTreeMap::new();
    // The runaway guard: tokens spent so far, resumed runs included.
    let mut budget = Spend {
        spent: transcript_token_total(&cfg.run_dir)?,
        cap: program.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET),
    };

    for (idx, step) in program.steps.iter().enumerate() {
        let state_path = cfg.run_dir.join(format!("step-{idx:02}-{}.json", step.name));
        if state_path.exists() {
            let text = fs::read_to_string(&state_path).map_err(|e| e.to_string())?;
            let artifact: Artifact =
                serde_json::from_str(&text).map_err(|e| format!("corrupt state {}: {e}", state_path.display()))?;
            artifacts.insert(step.name.clone(), artifact);
            continue;
        }
        if parked.contains(&step.name) {
            continue; // parked in an earlier invocation of this run
        }
        // A step whose input parked cannot proceed: the queue is blocked (exit 20).
        if let Some(dep) = step.inputs.iter().find(|i| parked.contains(*i)) {
            return Ok(Outcome::Blocked(format!(
                "step {:?} depends on parked step {:?}",
                step.name, dep
            )));
        }

        match step.opcode {
            Opcode::Generate => {
                // Same rule as gate steps (finding 6): gates verify the work.
                let gate_cwd = match &program.target {
                    Some(t) if !step.gate.is_empty() => ensure_worktree(cfg, t)?.path,
                    _ => cfg.repo_root.clone(),
                };
                match run_generate(cfg, step, idx, &template_root, &artifacts, transport, &mut budget, &gate_cwd)? {
                    Ok(artifact) => {
                        persist(&state_path, &artifact)?;
                        artifacts.insert(step.name.clone(), artifact);
                    }
                    Err(park) => {
                        append_parked(&cfg.run_dir, &park)?;
                        parked.push(step.name.clone());
                    }
                }
            }
            Opcode::Gate => {
                // Gates verify the WORK — with a target that's the worktree,
                // never the main checkout (finding 6).
                let gate_cwd = match &program.target {
                    Some(t) => ensure_worktree(cfg, t)?.path,
                    None => cfg.repo_root.clone(),
                };
                let report = run_gates(&step.gate, &gate_cwd, step.gate_timeout_s, &cfg.run_dir);
                if report.pass {
                    let artifact = Artifact {
                        kind: ArtifactKind::Json,
                        value: serde_json::to_value(&report).expect("GateReport serializes"),
                    };
                    persist(&state_path, &artifact)?;
                    artifacts.insert(step.name.clone(), artifact);
                } else {
                    append_parked(
                        &cfg.run_dir,
                        &ParkedItem {
                            step: step.name.clone(),
                            reason: format!(
                                "gate red: {}",
                                serde_json::to_string(&report).expect("GateReport serializes")
                            ),
                            attempts: 1,
                        },
                    )?;
                    parked.push(step.name.clone());
                }
            }
            Opcode::Escalate => {
                let esc_path = cfg.run_dir.join(format!("escalation-{}.md", step.name));
                if let Some(answer) = read_answer(&esc_path)? {
                    let artifact = Artifact {
                        kind: ArtifactKind::Text,
                        value: serde_json::Value::String(answer),
                    };
                    persist(&state_path, &artifact)?;
                    artifacts.insert(step.name.clone(), artifact);
                } else {
                    if !esc_path.exists() {
                        let question = render_step_template(step, &template_root, &cfg.repo_root, &artifacts)?;
                        fs::write(
                            &esc_path,
                            format!("# ESCALATION: {}\n\n{question}\n\n{ANSWER_MARKER}\n", step.name),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    return Ok(Outcome::Escalated(esc_path));
                }
            }
            Opcode::Execute => {
                let wt = ensure_worktree(cfg, program.target.as_ref().expect("validated: target present"))?;
                match run_execute(cfg, step, idx, &template_root, &artifacts, transport, &wt, &mut budget)? {
                    Ok(artifact) => {
                        persist(&state_path, &artifact)?;
                        artifacts.insert(step.name.clone(), artifact);
                    }
                    Err(park) => {
                        append_parked(&cfg.run_dir, &park)?;
                        parked.push(step.name.clone());
                    }
                }
            }
        }
    }
    Ok(Outcome::Done)
}

/// Generous by default — the guard exists for runaways, not normal runs
/// (Peter, 2026-07-29). The replay burned ~40K total for scale.
const DEFAULT_TOKEN_BUDGET: u64 = 500_000;

struct Spend {
    spent: u64,
    cap: u64,
}

impl Spend {
    /// Checked BEFORE each model call; one call may overrun (cost is unknown
    /// until the response), never two.
    fn check(&self) -> Result<(), String> {
        if self.spent >= self.cap {
            return Err(format!(
                "token budget exhausted ({}/{}) — the run is suspended; raise `token_budget` and rerun to resume",
                self.spent, self.cap
            ));
        }
        Ok(())
    }
    fn add(&mut self, result: &Result<crate::transport::CompletionResponse, crate::transport::TransportError>) {
        if let Ok(r) = result {
            // Sum EVERY HTTP post the transport made (internal retries and
            // fallbacks included) — hidden retries must not be free.
            for a in &r.attempts {
                self.spent += a["usage"]["total_tokens"].as_u64().unwrap_or(0);
            }
        }
    }
}

/// Resume support: what past invocations of this run already spent.
fn transcript_token_total(run_dir: &Path) -> Result<u64, String> {
    let path = run_dir.join("transcript.jsonl");
    if !path.exists() {
        return Ok(0);
    }
    let mut total = 0;
    let text = fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        match serde_json::from_str::<serde_json::Value>(line) {
            Ok(v) => {
                let attempts = v["response"]["attempts"].as_array().cloned().unwrap_or_default();
                if attempts.is_empty() {
                    total += v["response"]["usage"]["total_tokens"].as_u64().unwrap_or(0);
                } else {
                    for a in &attempts {
                        total += a["usage"]["total_tokens"].as_u64().unwrap_or(0);
                    }
                }
            }
            // A kill mid-append can tear exactly the LAST line — tolerate it
            // (finding 11); a torn line anywhere else is real corruption.
            Err(e) if i + 1 == lines.len() => {
                eprintln!("workflow: dropping torn trailing transcript line ({e})");
            }
            Err(e) => return Err(format!("corrupt transcript: {e}")),
        }
    }
    Ok(total)
}

/// The worktree is acquired once per run and remembered in run state (D6),
/// so a resumed run continues in the same tree.
fn ensure_worktree(cfg: &RunConfig, target: &Target) -> Result<Worktree, String> {
    let state = cfg.run_dir.join("worktree.json");
    if state.exists() {
        let text = fs::read_to_string(&state).map_err(|e| e.to_string())?;
        let wt: Worktree =
            serde_json::from_str(&text).map_err(|e| format!("corrupt worktree.json: {e}"))?;
        // The ring may have re-issued this slot since (finding 9).
        worktree::verify(&wt, target.branch.as_deref())?;
        return Ok(wt);
    }
    let wt = match &target.path {
        Some(path) => Worktree { path: path.clone(), slot: None },
        None => worktree::acquire(
            &cfg.repo_root,
            target.label.as_ref().expect("validated"),
            target.branch.as_ref().expect("validated"),
            target.tip.as_deref(),
        )?,
    };
    fs::write(&state, serde_json::to_string_pretty(&wt).expect("Worktree serializes")).map_err(|e| e.to_string())?;
    Ok(wt)
}

/// EXECUTE (D5): model emits a ChangeSet; the runtime applies it, commits with
/// a pathspec, runs the gate in the worktree, feeds failures back, cap then park.
fn run_execute(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    transport: &dyn ModelTransport,
    wt: &Worktree,
    budget: &mut Spend,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let mut feedback: Option<String> = None;
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        budget.check()?;
        // Re-rendered EVERY attempt: an earlier attempt may have committed,
        // and the model must quote the CURRENT worktree, not a stale excerpt
        // (finding 4 — stale prompts made every red gate a guaranteed park).
        // `file:` inputs read the WORKTREE, falling back nowhere.
        let base_prompt = render_step_template(step, template_root, &wt.path, artifacts)?;
        let user = match &feedback {
            None => base_prompt.clone(),
            Some(err) => format!("{base_prompt}\n\nYour previous attempt failed:\n{err}\nEmit a corrected ChangeSet."),
        };
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: execute has model"),
            max_tokens: step.max_tokens,
            system: None,
            user,
        };
        let result = transport.complete(&req);
        budget.add(&result);
        log_transcript(&cfg.run_dir, &step.name, idx, attempt, &req, &result)?;
        let error = match result {
            Err(e) => format!("transport error: {e}"),
            Ok(resp) => match Artifact::parse(ArtifactKind::ChangeSet, &resp.content) {
                Err(e) => e,
                Ok(artifact) => {
                    let change: ChangeSet =
                        serde_json::from_value(artifact.value.clone()).expect("parsed as ChangeSet above");
                    match worktree::apply(&wt.path, &change) {
                        Err(e) => e,
                        Ok(paths) => {
                            let sha = worktree::commit(&wt.path, &paths, &change.commit_message)?;
                            let report = run_gates(&step.gate, &wt.path, step.gate_timeout_s, &cfg.run_dir);
                            if report.pass {
                                let value = serde_json::json!({
                                    "change_set": artifact.value, "commit": sha,
                                    "worktree": wt.path, "attempt": attempt,
                                });
                                return Ok(Ok(Artifact { kind: ArtifactKind::Json, value }));
                            }
                            format!(
                                "your ChangeSet was applied and committed ({sha}), but the gate is red:\n{}",
                                serde_json::to_string_pretty(&report).expect("GateReport serializes")
                            )
                        }
                    }
                }
            },
        };
        feedback = Some(error);
    }
    Ok(Err(ParkedItem {
        step: step.name.clone(),
        reason: feedback.expect("at least one attempt ran"),
        attempts: max_attempts,
    }))
}

/// Ok(artifact) on success, Err(park) after the retry cap.
fn run_generate(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    transport: &dyn ModelTransport,
    budget: &mut Spend,
    gate_cwd: &Path,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let base_prompt = render_step_template(step, template_root, &cfg.repo_root, artifacts)?;
    let mut feedback: Option<String> = None;
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        budget.check()?;
        let user = match &feedback {
            None => base_prompt.clone(),
            Some(err) => format!("{base_prompt}\n\nYour previous attempt failed:\n{err}\nEmit a corrected response."),
        };
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: generate has model"),
            max_tokens: step.max_tokens,
            system: None,
            user,
        };
        let result = transport.complete(&req);
        budget.add(&result);
        log_transcript(&cfg.run_dir, &step.name, idx, attempt, &req, &result)?;
        let error = match result {
            Err(e) => format!("transport error: {e}"),
            Ok(resp) => match Artifact::parse(step.artifact, &resp.content) {
                Err(e) => e,
                Ok(artifact) => {
                    if step.gate.is_empty() {
                        return Ok(Ok(artifact));
                    }
                    let report = run_gates(&step.gate, gate_cwd, step.gate_timeout_s, &cfg.run_dir);
                    if report.pass {
                        return Ok(Ok(artifact));
                    }
                    format!(
                        "gate red: {}",
                        serde_json::to_string(&report).expect("GateReport serializes")
                    )
                }
            },
        };
        feedback = Some(error);
    }
    Ok(Err(ParkedItem {
        step: step.name.clone(),
        reason: feedback.expect("at least one attempt ran"),
        attempts: max_attempts,
    }))
}

fn render_step_template(
    step: &Step,
    template_root: &Path,
    repo_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<String, String> {
    let template_path = template_root.join(step.template.as_ref().expect("validated: template present"));
    let text = fs::read_to_string(&template_path)
        .map_err(|e| format!("cannot read template {}: {e}", template_path.display()))?;
    let mut inputs = BTreeMap::new();
    for input in &step.inputs {
        let value = if let Some(path) = input.strip_prefix("file:") {
            fs::read_to_string(repo_root.join(path))
                .map_err(|e| format!("step {:?} input file {path:?}: {e}", step.name))?
        } else {
            artifacts
                .get(input)
                .ok_or(format!("step {:?} input {input:?} has no artifact", step.name))?
                .render()
        };
        inputs.insert(input.clone(), value);
    }
    template::render(&text, &inputs).map_err(|e| format!("step {:?}: {e}", step.name))
}

/// INVARIANT: one transcript line per model request, retries included.
fn log_transcript(
    run_dir: &Path,
    step: &str,
    idx: usize,
    attempt: u32,
    req: &CompletionRequest,
    result: &Result<crate::transport::CompletionResponse, crate::transport::TransportError>,
) -> Result<(), String> {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let line = serde_json::json!({
        "step": step, "index": idx, "attempt": attempt, "ts": ts,
        "request": req,
        "response": match result {
            Ok(r) => serde_json::json!({"content": r.content, "usage": r.usage, "attempts": r.attempts}),
            Err(e) => serde_json::json!({"error": e.to_string()}),
        },
    });
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("transcript.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
}

/// Temp-file + rename: a kill mid-write must never leave a torn step JSON
/// (finding 11 — a torn artifact bricked every resume).
fn persist(path: &Path, artifact: &Artifact) -> Result<(), String> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(artifact).expect("artifact serializes"))
        .map_err(|e| e.to_string())?;
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn load_parked(run_dir: &Path) -> Result<Vec<ParkedItem>, String> {
    let path = run_dir.join("parked.jsonl");
    if !path.exists() {
        return Ok(Vec::new());
    }
    fs::read_to_string(&path)
        .map_err(|e| e.to_string())?
        .lines()
        .map(|l| serde_json::from_str(l).map_err(|e| format!("corrupt parked.jsonl: {e}")))
        .collect()
}

/// Remove a step's parked entry so a rerun retries it — the sanctioned
/// un-park (finding 2: parked was forever and the only escape was forbidden
/// hand-editing). A rerun of a parked step is a NEW SAMPLE by doctrine.
pub fn unpark(run_dir: &Path, step: &str) -> Result<(), String> {
    let items = load_parked(run_dir)?;
    if !items.iter().any(|p| p.step == step) {
        return Err(format!("step {step:?} is not parked (parked: {:?})", items.iter().map(|p| &p.step).collect::<Vec<_>>()));
    }
    let keep: Vec<String> = items
        .iter()
        .filter(|p| p.step != step)
        .map(|p| serde_json::to_string(p).expect("ParkedItem serializes"))
        .collect();
    let path = run_dir.join("parked.jsonl");
    if keep.is_empty() {
        fs::remove_file(&path).map_err(|e| e.to_string())
    } else {
        fs::write(&path, keep.join("\n") + "\n").map_err(|e| e.to_string())
    }
}

fn append_parked(run_dir: &Path, item: &ParkedItem) -> Result<(), String> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("parked.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", serde_json::to_string(item).expect("ParkedItem serializes")).map_err(|e| e.to_string())
}

fn read_answer(esc_path: &Path) -> Result<Option<String>, String> {
    if !esc_path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(esc_path).map_err(|e| e.to_string())?;
    // LAST occurrence: the runtime appends its marker at the END of the file,
    // so a question that QUOTES the marker text must not self-answer
    // (finding 1 — an unanswered escalation completed with garbage).
    let Some((_, after)) = text.rsplit_once(ANSWER_MARKER) else {
        return Err(format!("{} lost its answer marker", esc_path.display()));
    };
    let answer = after.trim();
    if answer.is_empty() { Ok(None) } else { Ok(Some(answer.to_string())) }
}
