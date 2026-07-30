//! Program files (WORKFLOW_RUNTIME_DESIGN.md D7): TOML, linear steps,
//! escalate is the only branch. Validation is load-time and loud.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::artifacts::ArtifactKind;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Program {
    pub name: String,
    /// Hard cap on total tokens across the whole run, retries included —
    /// the machine guard against runaway spend. Defaults to a generous
    /// 500K (Peter: sensible at first, must not block normal runs).
    /// Overrun suspends the run; raise and rerun to resume.
    pub token_budget: Option<u64>,
    /// Where execute steps land their commits. Required iff the program has one.
    pub target: Option<Target>,
    /// Opt-in: adjacent independent gate-less `generate` steps run threaded.
    /// Execute NEVER parallelizes (D-59: concurrent GPU gates flake).
    #[serde(default)]
    pub parallel: bool,
    /// Task/bead ID. When set, every completed verdict step is recorded in
    /// the shared decisions trail via `gate_runner review` (D8). Absent =
    /// verdicts stay in the run dir (toy/test programs never pollute it).
    pub task: Option<String>,
    /// Per-request transport deadline default for every model step. Sized
    /// for reasoning-tier latency on large prompts (the 2026-07-30 shakedown:
    /// deepseek-v4-pro thought past the old hardcoded 600s on a 40K-token
    /// brief and the connection was cut client-side). Step field overrides.
    pub request_timeout_s: Option<u64>,
    #[serde(rename = "step")]
    pub steps: Vec<Step>,
}

/// Either a pre-acquired worktree `path` (tests, replays into a prepared tree)
/// or ring acquisition by `label` + `branch` (+ optional `tip`). Exactly one.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub path: Option<PathBuf>,
    pub label: Option<String>,
    pub branch: Option<String>,
    pub tip: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Opcode {
    Generate,
    Execute,
    Gate,
    Escalate,
    /// Deterministic machine step: shell command reshapes artifacts, no model.
    Transform,
    /// One generate template over each element of a JSON-array input, collected.
    Fanout,
    /// k independent runs of a generate; gate picks the first pass, or a
    /// verdict majority decides. No model-driven control flow.
    Sample,
    /// Execute's semantics with a tool-using worker instead of a ChangeSet
    /// (D20): for output judged by RUNNING it, where a compiler must be in
    /// the loop.
    Lane,
}

impl Opcode {
    /// Execute and lane share the ONE target worktree and are inherently
    /// serial — the D15 block applies to both.
    pub fn touches_worktree(self) -> bool {
        matches!(self, Opcode::Execute | Opcode::Lane)
    }
}

/// What an execute step does when it exhausts `retry_cap` (D20). Promotion is
/// failure-driven only — there is no static complexity classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OnFail {
    Lane,
}

fn default_max_tokens() -> u32 {
    8192
}
/// Additional attempts after the first (D-52 carries 2 as the cap).
fn default_retry_cap() -> u8 {
    2
}
/// Enough turns for a real refactor-and-fix-forward pass; a wedged worker
/// still dies at `request_timeout_s`.
pub const DEFAULT_LANE_MAX_TURNS: u32 = 40;
/// The reserved cc-fleet id that runs the local claude CLI on the user's own
/// login — no provider row, no key material.
pub const DEFAULT_LANE_PROVIDER: &str = "claude";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
    /// Human-readable one-line sentence (Peter, 2026-07-30: names like "mb-a"
    /// are ciphers on a dashboard). Surfaced in status.json, `watch`, park
    /// records, and escalation files; `check` warns when absent.
    pub title: Option<String>,
    pub opcode: Opcode,
    pub model: Option<String>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default = "default_retry_cap")]
    pub retry_cap: u8,
    #[serde(default)]
    pub artifact: ArtifactKind,
    /// Prompt template path, relative to the program file's directory.
    pub template: Option<PathBuf>,
    /// Prior step names, or `file:<repo-relative path>` literals.
    #[serde(default)]
    pub inputs: Vec<String>,
    /// Shell commands; non-zero exit fails the attempt (generate) or the step (gate).
    #[serde(default)]
    pub gate: Vec<String>,
    /// Per-command timeout; a gate outliving it is killed and FAILS.
    #[serde(default = "default_gate_timeout")]
    pub gate_timeout_s: u64,
    /// Transform only: the shell command. Rendered template (if any) on stdin;
    /// stdout is the artifact. Non-zero exit parks — no retry, it's deterministic.
    pub command: Option<String>,
    /// Fanout only: the input (earlier step or `file:`) holding the JSON array.
    pub over: Option<String>,
    /// Sample only: number of independent runs (>= 2).
    pub samples: Option<u8>,
    /// Transport deadline for this step's model calls; falls back to the
    /// program-level `request_timeout_s`, then DEFAULT_REQUEST_TIMEOUT_S.
    /// Lane steps reuse it as the worker's wall timeout.
    pub request_timeout_s: Option<u64>,
    /// Lane (and a promoted execute): the cc-fleet provider positional.
    pub provider: Option<String>,
    /// Lane (and a promoted execute): cap on the worker's agentic turns.
    pub max_turns: Option<u32>,
    /// Execute only: exhausting `retry_cap` runs ONE lane attempt for the same
    /// step before parking, seeded with the accumulated error (D20).
    pub on_fail: Option<OnFail>,
    /// The model that promoted lane attempt uses; falls back to `model`.
    pub lane_model: Option<String>,
    /// Per-step token cap. Exceeding it parks THIS step and the run carries on;
    /// the run-wide `token_budget` suspends everything. Both stay in force,
    /// whichever hits first wins. (P3: one step ate 280K of a 400K run.)
    pub token_budget: Option<u64>,
}

/// 30 min: a reasoning-tier model on a whole-file prompt legitimately thinks
/// past 10 (observed: deepseek-v4-pro, 2026-07-30). A stuck call still dies.
pub const DEFAULT_REQUEST_TIMEOUT_S: u64 = 1800;

fn default_gate_timeout() -> u64 {
    crate::gates::DEFAULT_GATE_TIMEOUT_S
}

impl Program {
    pub fn load(path: &Path) -> Result<Program, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read program {}: {e}", path.display()))?;
        let mut program: Program =
            toml::from_str(&text).map_err(|e| format!("program {} is not valid: {e}", path.display()))?;
        // Back-fill so every consumer reads one place: the step field.
        for step in &mut program.steps {
            if step.request_timeout_s.is_none() {
                step.request_timeout_s = program.request_timeout_s;
            }
        }
        program.validate()?;
        Ok(program)
    }

    fn validate(&self) -> Result<(), String> {
        let mut seen: Vec<&str> = Vec::new();
        for step in &self.steps {
            if seen.contains(&step.name.as_str()) {
                return Err(format!("duplicate step name {:?}", step.name));
            }
            for input in step.inputs.iter().chain(&step.over) {
                if !input.starts_with("file:")
                    && !input.starts_with("anchor:")
                    && !seen.contains(&input.as_str())
                {
                    return Err(format!(
                        "step {:?} input {:?} names no earlier step (programs are linear)",
                        step.name, input
                    ));
                }
            }
            // Cross-opcode field misuse is loud at load time.
            if step.command.is_some() && step.opcode != Opcode::Transform {
                return Err(format!("step {:?}: `command` is transform-only", step.name));
            }
            if step.over.is_some() && step.opcode != Opcode::Fanout {
                return Err(format!("step {:?}: `over` is fanout-only", step.name));
            }
            if step.samples.is_some() && step.opcode != Opcode::Sample {
                return Err(format!("step {:?}: `samples` is sample-only", step.name));
            }
            if step.on_fail.is_some() && step.opcode != Opcode::Execute {
                return Err(format!(
                    "step {:?}: `on_fail` is execute-only — a lane has nothing to promote to",
                    step.name
                ));
            }
            if step.lane_model.is_some() && step.opcode != Opcode::Execute {
                return Err(format!("step {:?}: `lane_model` is execute-only (a lane uses `model`)", step.name));
            }
            for (field, set) in [("provider", step.provider.is_some()), ("max_turns", step.max_turns.is_some())] {
                if set && !step.opcode.touches_worktree() {
                    return Err(format!("step {:?}: `{field}` is lane-only", step.name));
                }
            }
            match step.opcode {
                Opcode::Generate => {
                    if step.model.is_none() || step.template.is_none() {
                        return Err(format!("generate step {:?} needs `model` and `template`", step.name));
                    }
                }
                Opcode::Execute => {
                    if step.model.is_none() || step.template.is_none() {
                        return Err(format!("execute step {:?} needs `model` and `template`", step.name));
                    }
                    if step.gate.is_empty() {
                        return Err(format!(
                            "execute step {:?} has no gate — an ungated execute is unreviewable",
                            step.name
                        ));
                    }
                    self.validate_target(&step.name, "execute")?;
                }
                Opcode::Lane => {
                    if step.template.is_none() {
                        return Err(format!("lane step {:?} needs `template` (the worker's brief)", step.name));
                    }
                    if step.gate.is_empty() {
                        return Err(format!(
                            "lane step {:?} has no gate — an ungated lane is unreviewable",
                            step.name
                        ));
                    }
                    self.validate_target(&step.name, "lane")?;
                }
                Opcode::Gate => {
                    if step.gate.is_empty() {
                        return Err(format!("gate step {:?} has no commands", step.name));
                    }
                }
                Opcode::Escalate => {
                    if step.template.is_none() {
                        return Err(format!("escalate step {:?} needs `template` (the question)", step.name));
                    }
                }
                Opcode::Transform => {
                    if step.command.is_none() {
                        return Err(format!("transform step {:?} needs `command`", step.name));
                    }
                    if step.model.is_some() {
                        return Err(format!(
                            "transform step {:?} is deterministic — no `model`",
                            step.name
                        ));
                    }
                }
                Opcode::Fanout => {
                    if step.model.is_none() || step.template.is_none() || step.over.is_none() {
                        return Err(format!(
                            "fanout step {:?} needs `model`, `template`, and `over` (the JSON-array input)",
                            step.name
                        ));
                    }
                }
                Opcode::Sample => {
                    if step.model.is_none() || step.template.is_none() {
                        return Err(format!("sample step {:?} needs `model` and `template`", step.name));
                    }
                    if step.samples.unwrap_or(0) < 2 {
                        return Err(format!("sample step {:?} needs `samples` >= 2", step.name));
                    }
                    if step.gate.is_empty() && step.artifact != ArtifactKind::Verdict {
                        return Err(format!(
                            "sample step {:?} needs a `gate` to pick the winner, or artifact = \"verdict\" for a majority vote",
                            step.name
                        ));
                    }
                }
            }
            seen.push(&step.name);
        }
        Ok(())
    }

    /// Every worktree-touching opcode needs the one `[target]`, declared
    /// exactly one way.
    fn validate_target(&self, step: &str, opcode: &str) -> Result<(), String> {
        let Some(t) = &self.target else {
            return Err(format!("{opcode} step {step:?} needs a [target] table"));
        };
        let by_path = t.path.is_some();
        let by_ring = t.label.is_some() && t.branch.is_some();
        if by_path == by_ring {
            return Err("[target] is exactly one of `path` OR `label`+`branch`".to_string());
        }
        Ok(())
    }
}
