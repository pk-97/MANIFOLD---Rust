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
            for input in &step.inputs {
                if !input.starts_with("file:") && !seen.contains(&input.as_str()) {
                    return Err(format!(
                        "step {:?} input {:?} names no earlier step (programs are linear)",
                        step.name, input
                    ));
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
            }
            seen.push(&step.name);
        }
        Ok(())
    }
}
