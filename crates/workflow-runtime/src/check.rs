//! `workflow check` — the standalone linter. The authoring model validates its
//! own programs BEFORE a run spends tokens: schema, template resolution (both
//! directions), `file:` existence, `anchor:` resolution, cross-opcode field
//! misuse. ALL findings are collected, never just the first.
//!
//! `file:`/`anchor:` inputs of execute steps resolve against the WORKTREE at
//! run time; check resolves them against the repo root — a base-tip
//! approximation, which is exactly what the authoring model sees anyway.

use std::collections::BTreeMap;
use std::path::Path;

use crate::locate;
use crate::program::{Opcode, Program};
use crate::template;

pub fn check(program_path: &Path, repo_root: &Path) -> Vec<String> {
    let program = match Program::load(program_path) {
        Ok(p) => p,
        Err(e) => return vec![e],
    };
    let template_root = match program_path.parent() {
        Some(p) => p.to_path_buf(),
        None => return vec!["program path has no parent directory".to_string()],
    };
    let mut findings = Vec::new();
    for step in &program.steps {
        if let Some(template) = &step.template {
            let path = template_root.join(template);
            match std::fs::read_to_string(&path) {
                Err(e) => findings.push(format!("step {:?}: template {}: {e}", step.name, path.display())),
                Ok(text) => {
                    // Dummy-render: proves slot coverage in both directions
                    // without model calls or artifacts.
                    let mut inputs: BTreeMap<String, String> = step
                        .inputs
                        .iter()
                        .map(|i| (i.clone(), "dummy".to_string()))
                        .collect();
                    if step.opcode == Opcode::Fanout {
                        inputs.insert("item".to_string(), "dummy".to_string());
                    }
                    if let Err(e) = template::render(&text, &inputs) {
                        findings.push(format!("step {:?}: {e}", step.name));
                    }
                }
            }
        }
        for input in step.inputs.iter().chain(&step.over) {
            if let Some(path) = input.strip_prefix("file:") {
                if !repo_root.join(path).is_file() {
                    findings.push(format!("step {:?}: input file {path:?} does not exist", step.name));
                }
            } else if let Some(spec) = input.strip_prefix("anchor:")
                && let Err(e) = locate::resolve(repo_root, spec)
            {
                findings.push(format!("step {:?}: {e}", step.name));
            }
        }
        if let Some(cmd) = &step.command
            && cmd.trim().is_empty()
        {
            findings.push(format!("step {:?}: `command` is empty", step.name));
        }
        for gate in &step.gate {
            if gate.trim().is_empty() {
                findings.push(format!("step {:?}: empty gate command", step.name));
            }
        }
    }
    findings
}
