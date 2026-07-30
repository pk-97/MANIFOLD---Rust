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
use crate::lane::{CcFleetLane, LaneOutcome, LaneRequest, LaneWorker};
use crate::ledger;
use crate::locate;
use crate::program::{OnFail, Opcode, Program, Step, Target};
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

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ParkedItem {
    pub step: String,
    /// The most informative error seen — never the empty-ChangeSet note when
    /// a red gate or apply failure preceded it (P3 shakedown, 2026-07-30).
    pub reason: String,
    pub attempts: u32,
    /// The step's human-readable `title`, when the program gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Execute only: the last red gate's FULL report — the composed reason
    /// string alone lost it when a later attempt failed differently.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate_report: Option<serde_json::Value>,
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

/// `workflow watch`'s liveness check: the pid in `run.lock`, if the process
/// that holds it is still alive. `None` covers both "no lock" and "dead pid".
pub fn holder_alive(run_dir: &Path) -> Option<i32> {
    let pid: i32 = fs::read_to_string(run_dir.join("run.lock")).ok()?.trim().parse().ok()?;
    (pid > 0 && unsafe { libc_kill_probe(pid) }).then_some(pid)
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

/// The live entry point: real API transport, real cc-fleet lane worker.
pub fn run(cfg: &RunConfig, transport: &dyn ModelTransport) -> Result<Outcome, String> {
    run_with(cfg, transport, &CcFleetLane)
}

/// A run a human took over ends resumed or abandoned, never neither: a stale
/// `blocked` status outlived by hours the blocker it named (2026-07-30).
pub fn abandon(run_dir: &Path, reason: &str) -> Result<(), String> {
    if !run_dir.is_dir() {
        return Err(format!("no run dir at {}", run_dir.display()));
    }
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let record = serde_json::json!({"ts": ts, "reason": reason});
    fs::write(
        run_dir.join("abandoned.json"),
        serde_json::to_string_pretty(&record).expect("abandon record serializes"),
    )
    .map_err(|e| e.to_string())?;
    crate::status::emit(run_dir, |st| {
        st.state = "abandoned".into();
        st.detail = reason.to_string();
    });
    ledger::append(run_dir, "abandon", None, None, Some(reason))
}

/// `workflow run --reopen`: the abandonment is lifted and the decision to lift
/// it is on the trail.
pub fn reopen(run_dir: &Path) -> Result<(), String> {
    let path = run_dir.join("abandoned.json");
    if !path.exists() {
        return Err(format!("run {} is not abandoned — nothing to reopen", run_dir.display()));
    }
    fs::remove_file(&path).map_err(|e| e.to_string())?;
    ledger::append(run_dir, "reopen", None, None, None)
}

/// The recorded reason, when this run was abandoned.
fn abandoned_reason(run_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(run_dir.join("abandoned.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(value["reason"].as_str().unwrap_or("no reason recorded").to_string())
}

pub fn run_with(
    cfg: &RunConfig,
    transport: &dyn ModelTransport,
    lane_worker: &dyn LaneWorker,
) -> Result<Outcome, String> {
    let program = Program::load(&cfg.program_path)?;
    fs::create_dir_all(&cfg.run_dir).map_err(|e| format!("cannot create run dir: {e}"))?;
    if let Some(reason) = abandoned_reason(&cfg.run_dir) {
        return Err(format!(
            "run {} was abandoned: {reason} — pass --reopen to continue it",
            cfg.run_dir.display()
        ));
    }
    let _lock = RunLock::take(&cfg.run_dir)?;
    check_program_unchanged(cfg, &program)?;
    let template_root = cfg
        .program_path
        .parent()
        .ok_or("program path has no parent")?
        .to_path_buf();

    let total_steps = program.steps.len();
    crate::status::emit(&cfg.run_dir, |st| {
        st.state = "run-started".into();
        st.detail = format!("{total_steps} steps");
        st.total_steps = total_steps;
        st.token_budget = program.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET);
    });
    let mut parked: Vec<String> = load_parked(&cfg.run_dir)?.into_iter().map(|p| p.step).collect();
    let mut artifacts: BTreeMap<String, Artifact> = BTreeMap::new();
    // The runaway guard: tokens spent so far, resumed runs included.
    let mut budget = Spend {
        spent: transcript_token_total(&cfg.run_dir)?,
        cap: program.token_budget.unwrap_or(DEFAULT_TOKEN_BUDGET),
        usd: transcript_usd_total(&cfg.run_dir),
        step_spent: 0,
        step_cap: None,
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
            crate::status::emit(&cfg.run_dir, |st| {
                st.state = "blocked".into();
                st.detail = format!("step {:?} depends on parked step {:?}", step.name, dep);
            });
            return Ok(Outcome::Blocked(format!(
                "step {:?} depends on parked step {:?}",
                step.name, dep
            )));
        }
        // Execute and lane steps share the ONE target worktree and are
        // inherently serial: a parked one means every later one builds on a
        // broken base, no `inputs` edge required (P3 shakedown: the runner
        // advanced past a parked refactor and spent 80K tokens on top of a
        // broken shader).
        if step.opcode.touches_worktree()
            && let Some(earlier) = program.steps[..idx]
                .iter()
                .find(|s| s.opcode.touches_worktree() && parked.contains(&s.name))
        {
            let reason = format!(
                "step {:?} is blocked: earlier step {:?} parked in the shared worktree",
                step.name, earlier.name
            );
            crate::status::emit(&cfg.run_dir, |st| {
                st.state = "blocked".into();
                st.detail = reason.clone();
            });
            return Ok(Outcome::Blocked(reason));
        }

        // A step's own cap counts only its own attempts, so it resets here.
        budget.begin_step(step.token_budget);
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "starting".into();
            st.detail = String::new();
            st.step = step.name.clone();
            st.title = step.title.clone().unwrap_or_default();
            st.step_index = idx + 1;
            st.opcode = format!("{:?}", step.opcode).to_lowercase();
            st.model = step.model.clone().unwrap_or_default();
            st.attempt = 0;
            st.max_attempts = u32::from(step.retry_cap) + 1;
            st.last_error = String::new();
        });

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
                            title: step.title.clone(),
                            ..Default::default()
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
                        let title = step.title.as_ref().map(|t| format!(" — {t}")).unwrap_or_default();
                        fs::write(
                            &esc_path,
                            format!("# ESCALATION: {}{title}\n\n{question}\n\n{ANSWER_MARKER}\n", step.name),
                        )
                        .map_err(|e| e.to_string())?;
                    }
                    crate::status::emit(&cfg.run_dir, |st| {
                        st.state = "escalated".into();
                        st.detail = esc_path.display().to_string();
                    });
                    return Ok(Outcome::Escalated(esc_path));
                }
            }
            Opcode::Execute | Opcode::Lane => {
                let wt = ensure_worktree(cfg, program.target.as_ref().expect("validated: target present"))?;
                let outcome = match step.opcode {
                    Opcode::Lane => {
                        let seed = load_unpark_seed(&cfg.run_dir, &step.name);
                        run_lane(
                            cfg,
                            step,
                            idx,
                            &template_root,
                            &artifacts,
                            lane_worker,
                            &wt,
                            &mut budget,
                            LaneStart::Fresh(seed),
                        )?
                    }
                    _ => {
                        let one_shot =
                            run_execute(cfg, step, idx, &template_root, &artifacts, transport, &wt, &mut budget)?;
                        match (one_shot, step.on_fail) {
                            // D20: a refactor-shaped one-shot execute is a
                            // design smell, and the run only learns which it
                            // was by failing. Promotion is failure-driven —
                            // one lane attempt on the accumulated error.
                            (Err(park), Some(OnFail::Lane)) => {
                                ledger::append(
                                    &cfg.run_dir,
                                    "promote",
                                    Some(&step.name),
                                    None,
                                    Some(&park.reason),
                                )?;
                                run_lane(
                                    cfg,
                                    step,
                                    idx,
                                    &template_root,
                                    &artifacts,
                                    lane_worker,
                                    &wt,
                                    &mut budget,
                                    LaneStart::Promoted(park),
                                )?
                            }
                            (other, _) => other,
                        }
                    }
                };
                match outcome {
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
    crate::status::emit(&cfg.run_dir, |st| {
        st.state = "run-done".into();
        st.detail = String::new();
    });
    // Once per completion: an idempotent rerun of a finished run must not
    // stack duplicate entries on the trail.
    if ledger::read(&cfg.run_dir).last().map(|e| e.kind != "run-done").unwrap_or(true) {
        ledger::append(&cfg.run_dir, "run-done", None, None, None)?;
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
                title: step.title.clone(),
                ..Default::default()
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
/// side effects — attempts come back for ordered logging. The step's own token
/// cap is counted here, in the thread, because the shared `Spend` is only
/// updated after the join and could not stop a call mid-flight.
fn pure_generate_attempts(step: &Step, base_prompt: &str, transport: &dyn ModelTransport) -> ThreadResult {
    let mut attempts = Vec::new();
    let mut feedback: Option<String> = None;
    let mut step_spent = 0u64;
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        if let Some(cap) = step.token_budget
            && step_spent >= cap
        {
            let park = ParkedItem {
                step: step.name.clone(),
                reason: step_budget_reason(step_spent, cap),
                attempts: attempt - 1,
                title: step.title.clone(),
                ..Default::default()
            };
            return Ok((attempts, Err(park)));
        }
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: model present"),
            max_tokens: step.max_tokens,
            system: None,
            user: compose(base_prompt, &feedback),
            timeout_s: step.request_timeout_s.unwrap_or(crate::program::DEFAULT_REQUEST_TIMEOUT_S),
        };
        let result = checked_complete(transport, &step.name, &req)?;
        step_spent += response_tokens(&result);
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
        title: step.title.clone(),
        ..Default::default()
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
        Ok(Err(ParkedItem {
            step: step.name.clone(),
            reason,
            attempts,
            title: step.title.clone(),
            ..Default::default()
        }))
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
    let park0 = |reason: String| {
        Ok(Err(ParkedItem {
            step: step.name.clone(),
            reason,
            attempts: 0,
            title: step.title.clone(),
            ..Default::default()
        }))
    };
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
                    title: step.title.clone(),
                    ..Default::default()
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
        Err(e) => {
            return Ok(Err(ParkedItem {
                step: step.name.clone(),
                reason: e,
                attempts: 0,
                title: step.title.clone(),
                ..Default::default()
            }));
        }
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
        Ok(Err(ParkedItem {
            step: step.name.clone(),
            reason,
            attempts: u32::from(k),
            title: step.title.clone(),
            ..Default::default()
        }))
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
    /// Lane spend, in dollars. The envelope reports USD, not tokens, and the
    /// two are never folded together — a dollar is not a token.
    usd: f64,
    /// This step's own spend across its own attempts, and its own cap.
    step_spent: u64,
    step_cap: Option<u64>,
}

impl Spend {
    fn begin_step(&mut self, cap: Option<u64>) {
        self.step_spent = 0;
        self.step_cap = cap;
    }
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
    /// The per-step cap parks THIS step and the run carries on; the run-wide
    /// cap suspends everything. Checked alongside `check`, whichever hits first.
    fn step_over(&self) -> Option<String> {
        let cap = self.step_cap?;
        (self.step_spent >= cap).then(|| step_budget_reason(self.step_spent, cap))
    }
    fn add(&mut self, result: &Result<crate::transport::CompletionResponse, crate::transport::TransportError>) {
        let tokens = response_tokens(result);
        self.spent += tokens;
        self.step_spent += tokens;
    }
    fn add_usd(&mut self, usd: f64) {
        self.usd += usd;
    }
}

fn step_budget_reason(spent: u64, cap: u64) -> String {
    format!(
        "step token budget exhausted ({spent}/{cap}) — this step is parked and the run continues; raise the step's `token_budget`, then unpark"
    )
}

/// Sum EVERY HTTP post the transport made (internal retries and fallbacks
/// included) — hidden retries must not be free.
fn response_tokens(result: &Result<CompletionResponse, TransportError>) -> u64 {
    match result {
        Err(_) => 0,
        Ok(r) => r.attempts.iter().map(|a| a["usage"]["total_tokens"].as_u64().unwrap_or(0)).sum(),
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

/// Resume support for lane spend. Dollars, kept apart from tokens, and read
/// from the same one record — the transcript.
fn transcript_usd_total(run_dir: &Path) -> f64 {
    let Ok(text) = fs::read_to_string(run_dir.join("transcript.jsonl")) else {
        return 0.0;
    };
    text.lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter_map(|v| v["response"]["total_cost_usd"].as_f64())
        .sum()
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
/// An empty ChangeSet is a NON-attempt: it never overwrites the real error,
/// and the park reason is the most informative error seen (P3 shakedown).
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
    // A rerun after `workflow unpark` starts from the recorded park reason:
    // committed progress in the worktree is fixed forward, never re-attempted
    // blind. One sample deep — the seed is the LAST park only.
    let mut informative: Option<String> = load_unpark_seed(&cfg.run_dir, &step.name);
    let mut seeded = informative.is_some();
    let human_note = load_unpark_note(&cfg.run_dir, &step.name);
    let mut empty_note = false;
    let mut last_red_gate: Option<serde_json::Value> = None;

    // Gate-first on a seeded rerun (D19, P3 shakedown 2026-07-30): a previous
    // sample parked after committing real progress into the worktree.
    // Calling the model blind either burns a call re-doing already-complete
    // work (gate green) or hands it the STALE park-reason text instead of
    // the CURRENT gate state (gate red) — the empty-ChangeSet deadlock that
    // cost 45K tokens per blind lap in the live run. Run the gate BEFORE the
    // first call and skip straight to the right branch. `run_gates` is
    // infallible (a hung gate is a red TIMEOUT result, never an error), so
    // there is no "gate can't run" fallback to the stale seed text — the
    // fresh report always wins.
    if seeded {
        let report = run_gates(&step.gate, &wt.path, step.gate_timeout_s, &cfg.run_dir);
        if report.pass {
            let sha = worktree::head_sha(&wt.path)?;
            let value = serde_json::json!({
                // No ChangeSet exists for this sample — the worktree was
                // already complete when the pre-flight gate ran. Marked
                // explicitly rather than faking an empty edits: [].
                "change_set": {"already_complete": true},
                "commit": sha, "worktree": wt.path, "attempt": 0,
            });
            return Ok(Ok(Artifact { kind: ArtifactKind::Json, value }));
        }
        let report_json = serde_json::to_value(&report).expect("GateReport serializes");
        last_red_gate = Some(report_json.clone());
        informative = Some(serde_json::to_string_pretty(&report_json).expect("Value serializes"));
    }
    let max_attempts = u32::from(step.retry_cap) + 1;
    for attempt in 1..=max_attempts {
        budget.check()?;
        if let Some(reason) = budget.step_over() {
            return Ok(Err(ParkedItem {
                step: step.name.clone(),
                reason,
                attempts: attempt - 1,
                title: step.title.clone(),
                gate_report: last_red_gate,
            }));
        }
        // Re-rendered EVERY attempt: an earlier attempt may have committed,
        // and the model must quote the CURRENT worktree, not a stale excerpt
        // (finding 4 — stale prompts made every red gate a guaranteed park).
        // `file:`/`anchor:` inputs read the WORKTREE, falling back nowhere.
        let base_prompt = match render_step_template(step, template_root, &wt.path, artifacts) {
            Ok(p) => p,
            Err(e) => {
                return Ok(Err(ParkedItem {
                    step: step.name.clone(),
                    reason: e,
                    attempts: attempt - 1,
                    title: step.title.clone(),
                    gate_report: last_red_gate,
                }));
            }
        };
        let mut user = base_prompt.clone();
        match (&informative, seeded) {
            (Some(err), true) => {
                // Reached only via the gate-first pre-flight above: `err` is
                // the FRESH gate report, not the stale unpark-seed text.
                user.push_str(&format!(
                    "\n\nWork already committed in the worktree stands. The gate is currently red:\n{err}\nFix forward from the current file contents."
                ));
            }
            (Some(err), false) => {
                user.push_str(&format!("\n\nYour previous attempt failed:\n{err}"));
            }
            (None, _) => {}
        }
        if empty_note {
            user.push_str(
                "\n\nYour last response was an EMPTY ChangeSet (no edits, no writes) — that is not an attempt. Emit real edits or writes; never an empty set.",
            );
        }
        if informative.is_some() || empty_note {
            user.push_str("\nEmit a corrected ChangeSet.");
        }
        if let Some(note) = &human_note {
            // Survives D19's staleness discard — see `load_unpark_note`.
            user.push_str(&format!("\n\nThe human who unparked this step said:\n{note}"));
        }
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: execute has model"),
            max_tokens: step.max_tokens,
            system: None,
            user,
            timeout_s: step.request_timeout_s.unwrap_or(crate::program::DEFAULT_REQUEST_TIMEOUT_S),
        };
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "waiting-on-model".into();
            // Attempt lives in the structured fields only — `detail` repeating it
            // double-printed in `watch`.
            st.detail = format!("{} char prompt", crate::status::commas(req.user.len() as u64));
            st.attempt = attempt;
            st.max_attempts = max_attempts;
        });
        let result = checked_complete(transport, &step.name, &req)?;
        budget.add(&result);
        crate::status::emit(&cfg.run_dir, |st| {
            st.tokens_spent = budget.spent;
        });
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
                            let report_json =
                                serde_json::to_value(&report).expect("GateReport serializes");
                            let msg = format!(
                                "your ChangeSet was applied and committed ({sha}), but the gate is red:\n{}",
                                serde_json::to_string_pretty(&report_json).expect("Value serializes")
                            );
                            last_red_gate = Some(report_json);
                            msg
                        }
                    }
                }
            },
        };
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "retrying".into();
            st.last_error = error.chars().take(300).collect();
        });
        if error == crate::artifacts::EMPTY_CHANGESET_ERR {
            // Non-attempt: the real error stays in front of the model and in
            // the park record; only the empty-set note is added.
            empty_note = true;
        } else {
            informative = Some(error);
            seeded = false;
            empty_note = false;
        }
    }
    let reason = informative.unwrap_or_else(|| {
        "every attempt returned an empty ChangeSet (no edits and no writes)".to_string()
    });
    Ok(Err(ParkedItem {
        step: step.name.clone(),
        reason,
        attempts: max_attempts,
        title: step.title.clone(),
        gate_report: last_red_gate,
    }))
}

/// Where a lane attempt starts. `Fresh` is a lane step (carrying its unpark
/// seed, if any); `Promoted` is an execute step that exhausted its retry cap,
/// carrying everything its one-shot samples learned.
enum LaneStart {
    Fresh(Option<String>),
    Promoted(ParkedItem),
}

/// LANE (D20): execute's contract with a tool-using worker. The runtime still
/// owns every side effect that decides anything — it enumerates what changed,
/// commits with a pathspec, runs the gate, feeds the red report back, caps the
/// attempts and parks. What the worker gains is a compiler inside its loop,
/// which is the whole difference between output judged by reading it and
/// output judged by running it.
#[allow(clippy::too_many_arguments)] // un-suppressed when the loop grows a params struct
fn run_lane(
    cfg: &RunConfig,
    step: &Step,
    idx: usize,
    template_root: &Path,
    artifacts: &BTreeMap<String, Artifact>,
    worker: &dyn LaneWorker,
    wt: &Worktree,
    budget: &mut Spend,
    start: LaneStart,
) -> Result<Result<Artifact, ParkedItem>, String> {
    let promoted = matches!(start, LaneStart::Promoted(_));
    let (mut informative, mut seeded, mut last_red_gate, max_attempts, one_shot_reason) = match start {
        LaneStart::Fresh(seed) => {
            let seeded = seed.is_some();
            (seed, seeded, None, u32::from(step.retry_cap) + 1, None)
        }
        // Exactly one attempt: promotion is the escape hatch from a failed
        // tier, not a second retry ladder.
        LaneStart::Promoted(park) => (Some(park.reason.clone()), false, park.gate_report, 1, Some(park.reason)),
    };
    let human_note = load_unpark_note(&cfg.run_dir, &step.name);
    let park = |reason: String, attempts: u32, gate_report: Option<serde_json::Value>| ParkedItem {
        step: step.name.clone(),
        reason,
        attempts,
        title: step.title.clone(),
        gate_report,
    };

    // Gate-first on a seeded rerun (D19), unchanged: the seed describes a
    // worktree that has since moved; the gate measures the one it is now.
    if seeded {
        let report = run_gates(&step.gate, &wt.path, step.gate_timeout_s, &cfg.run_dir);
        if report.pass {
            let sha = worktree::head_sha(&wt.path)?;
            let value = serde_json::json!({
                "lane": {"already_complete": true},
                "commit": sha, "worktree": wt.path, "attempt": 0,
            });
            return Ok(Ok(Artifact { kind: ArtifactKind::Json, value }));
        }
        let report_json = serde_json::to_value(&report).expect("GateReport serializes");
        informative = Some(serde_json::to_string_pretty(&report_json).expect("Value serializes"));
        last_red_gate = Some(report_json);
    }

    let mut no_change_note = false;
    for attempt in 1..=max_attempts {
        budget.check()?;
        if let Some(reason) = budget.step_over() {
            return Ok(Err(park(reason, attempt - 1, last_red_gate)));
        }
        // Re-rendered every attempt against the WORKTREE: the previous attempt
        // may have committed, and the brief must quote what is there now.
        let mut user = match render_step_template(step, template_root, &wt.path, artifacts) {
            Ok(p) => p,
            Err(e) => return Ok(Err(park(e, attempt - 1, last_red_gate))),
        };
        match (&informative, seeded, promoted) {
            (Some(err), _, true) => user.push_str(&format!(
                "\n\nOne-shot attempts at this step already failed. Any work they committed in the worktree stands:\n{err}\nFix forward from the current file contents."
            )),
            (Some(err), true, _) => user.push_str(&format!(
                "\n\nWork already committed in the worktree stands. The gate is currently red:\n{err}\nFix forward from the current file contents."
            )),
            (Some(err), false, _) => user.push_str(&format!("\n\nYour previous attempt failed:\n{err}")),
            (None, _, _) => {}
        }
        if no_change_note {
            user.push_str(
                "\n\nYour last run left the worktree completely unchanged — that is not an attempt. Edit the files.",
            );
        }
        if let Some(note) = &human_note {
            // The human's reasoning survives D19's staleness discard: it is
            // about what to do next, not about the state the gate re-measured.
            user.push_str(&format!("\n\nThe human who unparked this step said:\n{note}"));
        }
        let req = LaneRequest {
            worktree: wt.path.clone(),
            prompt: user,
            model: lane_model(step, promoted),
            provider: step.provider.clone().unwrap_or_else(|| crate::program::DEFAULT_LANE_PROVIDER.to_string()),
            max_turns: step.max_turns.unwrap_or(crate::program::DEFAULT_LANE_MAX_TURNS),
            timeout_s: step.request_timeout_s.unwrap_or(crate::program::DEFAULT_REQUEST_TIMEOUT_S),
        };
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "waiting-on-lane".into();
            st.detail = format!("{} char brief", crate::status::commas(req.prompt.len() as u64));
            st.attempt = attempt;
            st.max_attempts = max_attempts;
        });
        // A worker that commits its own work leaves a clean tree but a moved
        // HEAD — that is a real attempt, not a no-op.
        let head_before = worktree::head_sha(&wt.path)?;
        let result = checked_lane(worker, &step.name, &req)?;
        if let Ok(outcome) = &result {
            budget.add_usd(outcome.usd);
        }
        crate::status::emit(&cfg.run_dir, |st| {
            st.usd_spent = budget.usd;
        });
        log_lane_transcript(&cfg.run_dir, &step.name, idx, attempt, &req, &result)?;
        let error: Option<String> = match result {
            Err(e) => Some(e),
            Ok(outcome) if !outcome.ok => Some(outcome.error),
            Ok(outcome) => {
                let paths = worktree::changed_paths(&wt.path)?;
                let head_after = worktree::head_sha(&wt.path)?;
                let sha = if !paths.is_empty() {
                    let message =
                        format!("{} (lane attempt {attempt})", step.title.as_deref().unwrap_or(&step.name));
                    Some(worktree::commit(&wt.path, &paths, &message)?)
                } else if head_after != head_before {
                    Some(head_after)
                } else {
                    None
                };
                match sha {
                    // The lane analogue of an empty ChangeSet (D16): a
                    // non-attempt that never overwrites the real error.
                    None => {
                        no_change_note = true;
                        None
                    }
                    Some(sha) => {
                        let report = run_gates(&step.gate, &wt.path, step.gate_timeout_s, &cfg.run_dir);
                        if report.pass {
                            let mut value = serde_json::json!({
                                "lane": outcome.envelope, "commit": sha,
                                "worktree": wt.path, "attempt": attempt,
                            });
                            if promoted {
                                value["promoted"] = serde_json::Value::Bool(true);
                            }
                            return Ok(Ok(Artifact { kind: ArtifactKind::Json, value }));
                        }
                        let report_json = serde_json::to_value(&report).expect("GateReport serializes");
                        let msg = format!(
                            "your work was committed ({sha}), but the gate is red:\n{}",
                            serde_json::to_string_pretty(&report_json).expect("Value serializes")
                        );
                        last_red_gate = Some(report_json);
                        Some(msg)
                    }
                }
            }
        };
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "retrying".into();
            st.last_error = error
                .clone()
                .unwrap_or_else(|| "lane left the worktree unchanged".to_string())
                .chars()
                .take(300)
                .collect();
        });
        if let Some(e) = error {
            informative = Some(e);
            seeded = false;
            no_change_note = false;
        }
    }
    let reason = informative.unwrap_or_else(|| "every lane attempt left the worktree unchanged".to_string());
    let reason = match one_shot_reason {
        Some(one_shot) => {
            format!("one-shot execute exhausted its retry cap: {one_shot}\n\nthe promoted lane then failed: {reason}")
        }
        None => reason,
    };
    Ok(Err(park(reason, max_attempts, last_red_gate)))
}

/// A promoted execute names its lane model separately (`lane_model`) because
/// the tier that failed one-shot is rarely the tier worth handing tools.
fn lane_model(step: &Step, promoted: bool) -> Option<String> {
    if promoted { step.lane_model.clone().or_else(|| step.model.clone()) } else { step.model.clone() }
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
        Err(e) => {
            return Ok(Err(ParkedItem {
                step: step.name.clone(),
                reason: e,
                attempts: 0,
                title: step.title.clone(),
                ..Default::default()
            }));
        }
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
        if let Some(reason) = budget.step_over() {
            return Ok(Err(ParkedItem {
                step: label.to_string(),
                reason,
                attempts: attempt - 1,
                title: step.title.clone(),
                ..Default::default()
            }));
        }
        let req = CompletionRequest {
            model: step.model.clone().expect("validated: model present"),
            max_tokens: step.max_tokens,
            system: None,
            user: compose(base_prompt, &feedback),
            timeout_s: step.request_timeout_s.unwrap_or(crate::program::DEFAULT_REQUEST_TIMEOUT_S),
        };
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "waiting-on-model".into();
            // Attempt lives in the structured fields only — `detail` repeating it
            // double-printed in `watch`.
            st.detail = format!("{} char prompt", crate::status::commas(req.user.len() as u64));
            st.attempt = attempt;
            st.max_attempts = max_attempts;
        });
        let result = checked_complete(transport, label, &req)?;
        budget.add(&result);
        crate::status::emit(&cfg.run_dir, |st| {
            st.tokens_spent = budget.spent;
        });
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
        crate::status::emit(&cfg.run_dir, |st| {
            st.state = "retrying".into();
            st.last_error = error.chars().take(300).collect();
        });
        feedback = Some(error);
    }
    Ok(Err(ParkedItem {
        step: label.to_string(),
        reason: feedback.expect("at least one attempt ran"),
        attempts: max_attempts,
        title: step.title.clone(),
        ..Default::default()
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

/// The same choke point for the lane path: a brief is outbound context too,
/// and a gate tail fed back into one can leak a key just as easily.
fn checked_lane(
    worker: &dyn LaneWorker,
    label: &str,
    req: &LaneRequest,
) -> Result<Result<LaneOutcome, String>, String> {
    scrub::check(&req.prompt).map_err(|e| {
        format!("secret-shaped text in step {label:?}'s outbound context: {e} — run aborted; scrub the source, then rerun")
    })?;
    Ok(worker.run(req))
}

/// INVARIANT: one transcript line per lane launch, retries included. Lane cost
/// is dollars, so it rides in its own field — never summed into tokens.
fn log_lane_transcript(
    run_dir: &Path,
    step: &str,
    idx: usize,
    attempt: u32,
    req: &LaneRequest,
    result: &Result<LaneOutcome, String>,
) -> Result<(), String> {
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let line = serde_json::json!({
        "step": step, "index": idx, "attempt": attempt, "ts": ts,
        "request": req,
        "response": match result {
            Ok(o) => serde_json::json!({"ok": o.ok, "envelope": o.envelope, "total_cost_usd": o.usd}),
            Err(e) => serde_json::json!({"error": e}),
        },
    });
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("transcript.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())
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

fn unpark_seed_path(run_dir: &Path, step: &str) -> PathBuf {
    run_dir.join(format!("unpark-seed-{step}.txt"))
}

fn unpark_note_path(run_dir: &Path, step: &str) -> PathBuf {
    run_dir.join(format!("unpark-note-{step}.txt"))
}

/// The reason a previously-parked sample recorded, if the step was unparked.
/// Not consumed on read — a crashed rerun keeps its seed; each `unpark`
/// overwrites it (one sample deep, no accumulating history).
fn load_unpark_seed(run_dir: &Path, step: &str) -> Option<String> {
    fs::read_to_string(unpark_seed_path(run_dir, step)).ok().filter(|s| !s.trim().is_empty())
}

/// What the human decided when they unparked. Kept apart from the seed because
/// the gate-first pre-flight (D19) discards the stale park reason and this must
/// survive that: their reasoning is about what to do, not about what the gate
/// just re-measured.
fn load_unpark_note(run_dir: &Path, step: &str) -> Option<String> {
    fs::read_to_string(unpark_note_path(run_dir, step)).ok().filter(|s| !s.trim().is_empty())
}

/// Remove a step's parked entry so a rerun retries it — the sanctioned
/// un-park (finding 2: parked was forever and the only escape was forbidden
/// hand-editing). A rerun of a parked step is a NEW SAMPLE by doctrine.
/// The recorded park reason is left as a seed so the rerun's first attempt
/// sees what went wrong instead of starting blind (P3 shakedown), and `note`
/// — what the human decided that makes this retry worth running — rides with
/// it and lands on the decision trail.
pub fn unpark(run_dir: &Path, step: &str, note: &str) -> Result<(), String> {
    if note.trim().is_empty() {
        return Err("unpark needs a --note: what did you decide that makes this retry worth running?".to_string());
    }
    let items = load_parked(run_dir)?;
    let Some(item) = items.iter().find(|p| p.step == step) else {
        return Err(format!("step {step:?} is not parked (parked: {:?})", items.iter().map(|p| &p.step).collect::<Vec<_>>()));
    };
    fs::write(unpark_seed_path(run_dir, step), &item.reason).map_err(|e| e.to_string())?;
    fs::write(unpark_note_path(run_dir, step), note).map_err(|e| e.to_string())?;
    ledger::append(run_dir, "unpark", Some(step), Some(note), Some(&item.reason))?;
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

/// The one place a park is recorded — so it is also the one place the decision
/// trail learns about one.
fn append_parked(run_dir: &Path, item: &ParkedItem) -> Result<(), String> {
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join("parked.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(f, "{}", serde_json::to_string(item).expect("ParkedItem serializes")).map_err(|e| e.to_string())?;
    ledger::append(run_dir, "park", Some(&item.step), None, Some(&item.reason))
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
