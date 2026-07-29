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
}

fn default_max_tokens() -> u32 {
    8192
}
/// Additional attempts after the first (D-52 carries 2 as the cap).
fn default_retry_cap() -> u8 {
    2
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub name: String,
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
}

fn default_gate_timeout() -> u64 {
    crate::gates::DEFAULT_GATE_TIMEOUT_S
}

impl Program {
    pub fn load(path: &Path) -> Result<Program, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read program {}: {e}", path.display()))?;
        let program: Program =
            toml::from_str(&text).map_err(|e| format!("program {} is not valid: {e}", path.display()))?;
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
                    match &self.target {
                        None => {
                            return Err(format!("execute step {:?} needs a [target] table", step.name));
                        }
                        Some(t) => {
                            let by_path = t.path.is_some();
                            let by_ring = t.label.is_some() && t.branch.is_some();
                            if by_path == by_ring {
                                return Err(
                                    "[target] is exactly one of `path` OR `label`+`branch`".to_string()
                                );
                            }
                        }
                    }
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
}
