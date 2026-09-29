//! Whole-project undo/redo oracle for setter-style commands.
//!
//! Compared on the serialized project, which is exactly what a save persists.
//! Shared by this crate's unit tests and `tests/command_roundtrips.rs` (pulled
//! in with `#[path]`), so `Command` is named through the includer's `super`.

use manifold_core::project::Project;
use serde_json::Value;

use super::Command;

pub struct SetterCase {
    pub name: &'static str,
    /// Seed the project with whatever the command needs, then build it.
    pub build: fn(&mut Project) -> Box<dyn Command>,
    /// True once the field the command exists to change holds its new value.
    pub applied: fn(&Project) -> bool,
}

/// JSON pointers into the serialized project that may legitimately differ
/// after undo. Version counters only; anything else differing is a bug.
const EXCLUDED: &[&str] = &[];

fn snapshot(project: &Project) -> Value {
    let mut value = serde_json::to_value(project).expect("project serializes");
    for pointer in EXCLUDED {
        if let Some(field) = value.pointer_mut(pointer) {
            *field = Value::Null;
        }
    }
    value
}

fn first_difference(a: &Value, b: &Value, path: &mut String) -> Option<String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            for key in x.keys().chain(y.keys().filter(|k| !x.contains_key(*k))) {
                let len = path.len();
                path.push('/');
                path.push_str(key);
                let found = match (x.get(key), y.get(key)) {
                    (Some(l), Some(r)) => first_difference(l, r, path),
                    (l, r) => Some(format!("{path}: {l:?} vs {r:?}")),
                };
                if found.is_some() {
                    return found;
                }
                path.truncate(len);
            }
            None
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (i, (l, r)) in x.iter().zip(y).enumerate() {
                let len = path.len();
                path.push_str(&format!("/{i}"));
                if let Some(found) = first_difference(l, r, path) {
                    return Some(found);
                }
                path.truncate(len);
            }
            None
        }
        _ if a == b => None,
        _ => Some(format!("{path}: {a} vs {b}")),
    }
}

fn check(case: &SetterCase, fresh: fn() -> Project) -> Result<(), String> {
    let mut project = fresh();
    let mut cmd = (case.build)(&mut project);
    let before = snapshot(&project);

    cmd.execute(&mut project);
    if !(case.applied)(&project) {
        return Err("execute did not apply".into());
    }
    let executed = snapshot(&project);
    if before == executed {
        return Err("execute left the project unchanged".into());
    }

    cmd.undo(&mut project);
    if let Some(diff) = first_difference(&before, &snapshot(&project), &mut String::new()) {
        return Err(format!("undo diverged at {diff}"));
    }

    cmd.execute(&mut project);
    if let Some(diff) = first_difference(&executed, &snapshot(&project), &mut String::new()) {
        return Err(format!("redo diverged at {diff}"));
    }
    Ok(())
}

/// Per case: execute applies the change, undo restores the whole project, and
/// redo reproduces the executed project exactly. Reports every failing case.
pub fn assert_setter_cases(fresh: fn() -> Project, cases: &[SetterCase]) {
    let failures: Vec<String> = cases
        .iter()
        .filter_map(|case| check(case, fresh).err().map(|e| format!("{}: {e}", case.name)))
        .collect();
    assert!(failures.is_empty(), "{} case(s) failed:\n{}", failures.len(), failures.join("\n"));
}
