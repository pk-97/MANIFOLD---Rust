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

use crate::artifacts::{Artifact, ArtifactKind, ChangeSet, Verdict};
use crate::gates::{run_gates, run_gates_env, run_transform};
use crate::locate;
use crate::program::{Opcode, Program, Step, Target};
use crate::scrub;
use crate::template;
use crate::transport::{CompletionRequest, CompletionResponse, ModelTransport, TransportError};
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

    let mut idx = 0;
    while idx < program.steps.len() {
        let step = &program.steps[idx];
        let state_path = state_path_for(&cfg.run_dir, idx, &step.name);
        if state_path.exists() {
            let text = fs::read_to_string(&state_path).map_err(|e| e.to_string())?;
            let artifact: Artifact =
                serde_json::from_str(&text).map_err(|e| format!("corrupt state {}: {e}", state_path.display()))?;
            artifacts.insert(step.name.clone(), artifact);
            idx += 1;
            continue;
        }
        if parked.contains(&step.name) {
            idx += 1;
            continue; // parked in an earlier invocation of this run
        }
        // A step whose input parked cannot proceed: the queue is blocked (exit 20).
        if let Some(dep) = step.inputs.iter().find(|i| parked.contains(*i)) {
            return Ok(Outcome::Blocked(format!(
                "step {:?} depends on parked step {:?}",
                step.name, dep
            )));
        }

        // Parallel generate (v1.1, opt-in): adjacent gate-less generates with
        // no artifact edges between them run threaded. Execute NEVER
        // parallelizes (D-59: concurrent GPU gates flake).
        if program.parallel && pure_generate(step) {
            let batch = collect_parallel_batch(cfg, &program, idx, &parked);
            if batch.len() >= 2 {
                run_parallel_generates(
                    cfg,
                    &program,
                    &batch,
                    &template_root,
                    &mut artifacts,
                    transport,
                    &mut budget,
                    &mut parked,
                )?;
                idx += batch.len();
                continue;
            }
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
                        record_verdict_if_due(cfg, &program, step, &artifact)?;
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
            Opcode::Transform => {
                match run_transform_step(cfg, step, &template_root, &artifacts)? {
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
            Opcode::Fanout | Opcode::Sample => {
                let gate_cwd = match &program.target {
                    Some(t) if !step.gate.is_empty() => ensure_worktree(cfg, t)?.path,
                    _ => cfg.repo_root.clone(),
                };
                let outcome = match step.opcode {
                    Opcode::Fanout => {
                        run_fanout(cfg, step, idx, &template_root, &artifacts, transport, &mut budget, &gate_cwd)?
                    }
                    _ => run_sample(cfg, step, idx, &template_root, &artifacts, transport, &mut budget, &gate_cwd)?,
                };
                match outcome {
                    Ok(artifact) => {
                        persist(&state_path, &artifact)?;
                        record_verdict_if_due(cfg, &program, step, &artifact)?;
                        artifacts.insert(step.name.clone(), artifact);
                    }
                    Err(park) => {
                        append_parked(&cfg.run_dir, &park)?;
                        parked.push(step.name.clone());
                    }
                }
            }
        }
        idx += 1;
    }
    Ok(Outcome::Done)
}

/// D8: a completed verdict step in a program that carries a `task` goes on
/// the shared decisions trail, with the MODEL as the reviewing seat. No task,
/// no write — toy runs never pollute decisions.md. Recording failure is a
/// hard run error: an off-the-record verdict must never look green.
fn record_verdict_if_due(
    cfg: &RunConfig,
    program: &Program,
    step: &Step,
    artifact: &Artifact,
) -> Result<(), String> {
    if artifact.kind != ArtifactKind::Verdict {
        return Ok(());
    }
    let Some(task) = &program.task else { return Ok(()) };
    let verdict: Verdict =
        serde_json::from_value(artifact.value.clone()).map_err(|e| format!("stored Verdict re-parse: {e}"))?;
    crate::gates::record_review(
        &cfg.repo_root,
        task,
        &verdict.verdict,
        &format!("{} step {}", program.name, step.name),
        &verdict.rationale,
        step.model.as_deref().unwrap_or("model"),
    )
}

fn state_path_for(run_dir: &Path, idx: usize, name: &str) -> PathBuf {
    run_dir.join(format!("step-{idx:02}-{name}.json"))
}

fn pure_generate(step: &Step) -> bool {
    step.opcode == Opcode::Generate && step.gate.is_empty()
}

/// Consecutive not-yet-done pure generates from `start` whose inputs name no
/// step inside the batch — parallel-safe by construction.
fn collect_parallel_batch(cfg: &RunConfig, program: &Program, start: usize, parked: &[String]) -> Vec<usize> {
    let mut batch = vec![start];
    let mut names: Vec<&str> = vec![&program.steps[start].name];
    for (j, step) in program.steps.iter().enumerate().skip(start + 1) {
        let independent = step
            .inputs
            .iter()
            .all(|i| !names.contains(&i.as_str()) && !parked.contains(i));
        if !pure_generate(step)
            || state_path_for(&cfg.run_dir, j, &step.name).exists()
            || parked.contains(&step.name)
            || !independent
        {
            break;
        }
        batch.push(j);
        names.push(&step.name);
    }
    batch
}

/// One thread per batch member; transcript, budget, and state writes happen
/// AFTER the join, in step order — the run dir stays deterministic. The budget
/// is checked once for the batch, so a batch may overrun by its width (the
/// sequential loop's own bound is one call).
#[allow(clippy::too_many_arguments)] // un-suppressed when the loop grows a params struct
fn run_parallel_generates(
    cfg: &RunConfig,
    program: &Program,
    batch: &[usize],
    template_root: &Path,
    artifacts: &mut BTreeMap<String, Artifact>,
    transport: &dyn ModelTransport,
    budget: &mut Spend,
    parked: &mut Vec<String>,
) -> Result<(), String> {
    budget.check()?;
    // Render sequentially first: prompts see the pre-batch artifact map.
    let mut jobs: Vec<(usize, &Step, Result<String, String>)> = Vec::new();
    for &i in batch {
        let step = &program.steps[i];
        jobs.push((i, step, render_step_template(step, template_root, &cfg.repo_root, artifacts)));
    }
    let results: Vec<Option<ThreadResult>> = std::thread::scope(|scope| {
        let handles: Vec<_> = jobs
            .iter()
            .map(|(_, step, prompt)| {
                let Ok(prompt) = prompt else { return None };
                Some(scope.spawn(move || pure_generate_attempts(step, prompt, transport)))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.map(|h| h.join().expect("parallel generate thread panicked")))
            .collect()
    });
    for ((i, step, prompt), result) in jobs.iter().zip(results) {
        let state_path = state_path_for(&cfg.run_dir, *i, &step.name);
        let outcome = match result {
            None => Err(ParkedItem {
                step: step.name.clone(),
                reason: prompt.as_ref().expect_err("no thread means render failed").clone(),
                attempts: 0,
            }),
            Some(thread_result) => {
                let (attempts, outcome) = thread_result?; // scrub abort kills the run
                for (attempt, req, result) in &attempts {
                    budget.add(result);
                    log_transcript(&cfg.run_dir, &step.name, *i, *attempt, req, result)?;
                }
                outcome
            }
        };
        match outcome {
            Ok(artifact) => {
                persist(&state_path, &artifact)?;
                // A verdict step in a parallel batch is still a verdict (D8).
                record_verdict_if_due(cfg, program, step, &artifact)?;
                artifacts.insert(step.name.clone(), artifact);
            }
            Err(park) => {
                append_parked(&cfg.run_dir, &park)?;
                parked.push(step.name.clone());
            }
        }
    }
    Ok(())
}

/// One model attempt as data: (attempt number, request, response).
type Attempt = (u32, CompletionRequest, Result<CompletionResponse, TransportError>);
/// A parallel-generate thread's outcome; the outer Err is a scrub abort.
type ThreadResult = Result<(Vec<Attempt>, Result<Artifact, ParkedItem>), String>;

/// The thread body: the model_loop's compose/scrub/call/parse ladder without
/// side effects — attempts come back for ordered logging.
fn pure_generate_attempts(step: &Step, base_prompt: &str, transport: &dyn ModelTransport) -> ThreadResult {
    let mut attempts = Vec::new();
    let mut feedback: Option<String> = None;
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: model present"),
            max_tokens: step.max_tokens,
            system: None,
            user: compose(base_prompt, &feedback),
        };
        let result = checked_complete(transport, &step.name, &req)?;
        let error = match &result {
            Err(e) => Some(format!("transport error: {e}")),
            Ok(resp) => Artifact::parse(step.artifact, &resp.content).err(),
        };
        attempts.push((attempt, req, result));
        match error {
            None => {
                let (_, _, Ok(resp)) = attempts.last().expect("just pushed") else { unreachable!() };
                let artifact = Artifact::parse(step.artifact, &resp.content).expect("parsed above");
                return Ok((attempts, Ok(artifact)));
            }
            Some(e) => feedback = Some(e),
        }
    }
    let park = ParkedItem {
        step: step.name.clone(),
        reason: feedback.expect("at least one attempt ran"),
        attempts: max_attempts,
    };
    Ok((attempts, Err(park)))
}

/// TRANSFORM (v1.1): deterministic machine step — rendered template on stdin,
/// stdout parsed as the artifact. Failures park immediately: same input,
/// same output, a retry buys nothing.
fn run_transform_step(
    cfg: &RunConfig,
    step: &Step,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let park = |reason: String, attempts: u32| {
        Ok(Err(ParkedItem { step: step.name.clone(), reason, attempts }))
    };
    let input = match &step.template {
        Some(_) => match render_step_template(step, template_root, &cfg.repo_root, artifacts) {
            Ok(p) => p,
            Err(e) => return park(e, 0),
        },
        None => String::new(),
    };
    let cmd = step.command.as_ref().expect("validated: transform has command");
    match run_transform(cmd, &cfg.repo_root, &input, step.gate_timeout_s, &cfg.run_dir) {
        Err(e) => park(e, 1),
        Ok(stdout) => match Artifact::parse(step.artifact, &stdout) {
            Err(e) => park(format!("transform stdout does not parse: {e}"), 1),
            Ok(artifact) => Ok(Ok(artifact)),
        },
    }
}

/// FANOUT (v1.1): the same generate template over each element of a JSON-array
/// input, strictly sequential, collected into one array artifact. An element
/// failing its retry cap parks the WHOLE step — a partial collection is not
/// an artifact.
#[allow(clippy::too_many_arguments)] // un-suppressed when the loop grows a params struct
fn run_fanout(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    transport: &dyn ModelTransport,
    budget: &mut Spend,
    gate_cwd: &Path,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let park0 = |reason: String| Ok(Err(ParkedItem { step: step.name.clone(), reason, attempts: 0 }));
    let over = step.over.as_ref().expect("validated: fanout has over");
    let value: serde_json::Value = if let Some(path) = over.strip_prefix("file:") {
        match fs::read_to_string(cfg.repo_root.join(path)) {
            Ok(text) => match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(e) => return park0(format!("fanout input file {path:?} is not JSON: {e}")),
            },
            Err(e) => return park0(format!("fanout input file {path:?}: {e}")),
        }
    } else {
        artifacts.get(over).expect("validated: over names an earlier step").value.clone()
    };
    let Some(items) = value.as_array() else {
        return park0(format!("fanout input {over:?} is not a JSON array"));
    };
    let mut collected = Vec::with_capacity(items.len());
    for (i, item) in items.iter().enumerate() {
        let label = format!("{}[{i}]", step.name);
        let rendered_item = match item {
            serde_json::Value::String(s) => s.clone(),
            other => serde_json::to_string_pretty(other).expect("JSON value serializes"),
        };
        let base_prompt = match render_step_template_with(
            step,
            template_root,
            &cfg.repo_root,
            artifacts,
            &[("item", rendered_item)],
        ) {
            Ok(p) => p,
            Err(e) => return park0(e),
        };
        match model_loop(cfg, step, idx, &label, &base_prompt, transport, budget, gate_cwd, true)? {
            Ok(artifact) => collected.push(artifact.value),
            Err(element_park) => {
                return Ok(Err(ParkedItem {
                    step: step.name.clone(),
                    reason: format!("element {i} of {} parked: {}", items.len(), element_park.reason),
                    attempts: element_park.attempts,
                }));
            }
        }
    }
    Ok(Ok(Artifact { kind: ArtifactKind::Json, value: serde_json::Value::Array(collected) }))
}

/// SAMPLE (v1.1): k independent runs (rerun-is-a-new-sample, made a feature).
/// Selection is machine-only: the step's gate picks the first passing
/// candidate (`$WORKFLOW_SAMPLE` = candidate path), or verdict artifacts
/// take a strict majority. No winner is a park — never a model tiebreak.
#[allow(clippy::too_many_arguments)] // un-suppressed when the loop grows a params struct
fn run_sample(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    transport: &dyn ModelTransport,
    budget: &mut Spend,
    gate_cwd: &Path,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let base_prompt = match render_step_template(step, template_root, &cfg.repo_root, artifacts) {
        Ok(p) => p,
        Err(e) => return Ok(Err(ParkedItem { step: step.name.clone(), reason: e, attempts: 0 })),
    };
    let k = step.samples.expect("validated: sample has samples");
    let mut candidates: Vec<(u8, Artifact)> = Vec::new();
    let mut failures: Vec<String> = Vec::new();
    for i in 0..k {
        let label = format!("{}[sample-{i}]", step.name);
        match model_loop(cfg, step, idx, &label, &base_prompt, transport, budget, gate_cwd, false)? {
            Ok(artifact) => candidates.push((i, artifact)),
            Err(p) => failures.push(format!("sample {i}: {}", p.reason)),
        }
    }
    let park = |reason: String| {
        Ok(Err(ParkedItem { step: step.name.clone(), reason, attempts: u32::from(k) }))
    };
    if candidates.is_empty() {
        return park(format!("all {k} samples failed to parse: {}", failures.join(" | ")));
    }
    if !step.gate.is_empty() {
        for (i, artifact) in &candidates {
            let cand_path = cfg.run_dir.join(format!("sample-{}-{i}.json", step.name));
            fs::write(&cand_path, artifact.render()).map_err(|e| e.to_string())?;
            let report = run_gates_env(
                &step.gate,
                gate_cwd,
                step.gate_timeout_s,
                &cfg.run_dir,
                &[("WORKFLOW_SAMPLE", cand_path.as_path())],
            );
            if report.pass {
                return Ok(Ok(artifact.clone()));
            }
        }
        return park(format!("none of the {} parsed samples passed the gate", candidates.len()));
    }
    // Verdict vote (validated: gate-less sample means artifact = verdict).
    let verdict_of = |a: &Artifact| -> String {
        serde_json::from_value::<Verdict>(a.value.clone()).expect("parsed as Verdict").verdict
    };
    let accepts = candidates.iter().filter(|(_, a)| verdict_of(a) == "accept").count();
    let rejects = candidates.len() - accepts;
    if accepts == rejects {
        return park(format!("verdict vote tied {accepts}-{rejects} across {k} samples"));
    }
    let majority = if accepts > rejects { "accept" } else { "reject" };
    let (_, winner) = candidates
        .iter()
        .find(|(_, a)| verdict_of(a) == majority)
        .expect("majority side is non-empty");
    Ok(Ok(winner.clone()))
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
        // `file:`/`anchor:` inputs read the WORKTREE, falling back nowhere.
        let base_prompt = match render_step_template(step, template_root, &wt.path, artifacts) {
            Ok(p) => p,
            Err(e) => {
                return Ok(Err(ParkedItem { step: step.name.clone(), reason: e, attempts: attempt - 1 }));
            }
        };
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
        let result = checked_complete(transport, &step.name, &req)?;
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
    let base_prompt = match render_step_template(step, template_root, &cfg.repo_root, artifacts) {
        Ok(p) => p,
        // Render/resolve failures are deterministic — park without burning calls.
        Err(e) => return Ok(Err(ParkedItem { step: step.name.clone(), reason: e, attempts: 0 })),
    };
    model_loop(cfg, step, idx, &step.name, &base_prompt, transport, budget, gate_cwd, true)
}

/// The shared model-call loop: compose (base + feedback), scrub, call, parse,
/// optionally run the step's gate; feed errors back to the retry cap, then park.
/// `label` keys the transcript and park entries (fanout elements are "step[i]").
#[allow(clippy::too_many_arguments)] // un-suppressed when the loop grows a params struct
fn model_loop(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    label: &str,
    base_prompt: &str,
    transport: &dyn ModelTransport,
    budget: &mut Spend,
    gate_cwd: &Path,
    use_gate: bool,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let mut feedback: Option<String> = None;
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        budget.check()?;
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: model present"),
            max_tokens: step.max_tokens,
            system: None,
            user: compose(base_prompt, &feedback),
        };
        let result = checked_complete(transport, label, &req)?;
        budget.add(&result);
        log_transcript(&cfg.run_dir, label, idx, attempt, &req, &result)?;
        let error = match result {
            Err(e) => format!("transport error: {e}"),
            Ok(resp) => match Artifact::parse(step.artifact, &resp.content) {
                Err(e) => e,
                Ok(artifact) => {
                    if !use_gate || step.gate.is_empty() {
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
        step: label.to_string(),
        reason: feedback.expect("at least one attempt ran"),
        attempts: max_attempts,
    }))
}

fn compose(base: &str, feedback: &Option<String>) -> String {
    match feedback {
        None => base.to_string(),
        Some(err) => format!("{base}\n\nYour previous attempt failed:\n{err}\nEmit a corrected response."),
    }
}

fn render_step_template(
    step: &Step,
    template_root: &Path,
    repo_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
) -> Result<String, String> {
    render_step_template_with(step, template_root, repo_root, artifacts, &[])
}

fn render_step_template_with(
    step: &Step,
    template_root: &Path,
    repo_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    extra: &[(&str, String)],
) -> Result<String, String> {
    let template_path = template_root.join(step.template.as_ref().expect("validated: template present"));
    let text = fs::read_to_string(&template_path)
        .map_err(|e| format!("cannot read template {}: {e}", template_path.display()))?;
    let mut inputs = BTreeMap::new();
    for input in &step.inputs {
        let value = if let Some(path) = input.strip_prefix("file:") {
            fs::read_to_string(repo_root.join(path))
                .map_err(|e| format!("step {:?} input file {path:?}: {e}", step.name))?
        } else if let Some(spec) = input.strip_prefix("anchor:") {
            // Deterministic locate: symbol -> defining span, no model call.
            locate::resolve(repo_root, spec).map_err(|e| format!("step {:?}: {e}", step.name))?
        } else {
            artifacts
                .get(input)
                .ok_or(format!("step {:?} input {input:?} has no artifact", step.name))?
                .render()
        };
        inputs.insert(input.clone(), value);
    }
    for (key, value) in extra {
        inputs.insert((*key).to_string(), value.clone());
    }
    template::render(&text, &inputs).map_err(|e| format!("step {:?}: {e}", step.name))
}

/// The secrets choke point: NOTHING ships to a transport without this scan —
/// feedback loops included, a gate tail can leak a key too. A hit aborts the
/// run (exit 2): a secret in context is an authoring bug no retry fixes.
fn checked_complete(
    transport: &dyn ModelTransport,
    label: &str,
    req: &CompletionRequest,
) -> Result<Result<CompletionResponse, TransportError>, String> {
    for text in req.system.iter().chain(std::iter::once(&req.user)) {
        scrub::check(text).map_err(|e| {
            format!("secret-shaped text in step {label:?}'s outbound context: {e} — run aborted; scrub the source, then rerun")
        })?;
    }
    Ok(transport.complete(req))
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
