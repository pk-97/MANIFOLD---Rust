//! P1 gates (WORKFLOW_RUNTIME_DESIGN.md section 5, Phasing): retry cap, resume,
//! escalate, transcript completeness, park-vs-block — against the mock seam.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use workflow_runtime::runner::{Outcome, RunConfig, run};
use workflow_runtime::transport::MockTransport;

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new(name: &str, program: &str, templates: &[(&str, &str)]) -> Fixture {
        let root = std::env::temp_dir().join(format!("workflow-runtime-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("programs")).unwrap();
        fs::write(root.join("programs/program.toml"), program).unwrap();
        for (file, text) in templates {
            fs::write(root.join("programs").join(file), text).unwrap();
        }
        Fixture { root }
    }
    fn cfg(&self) -> RunConfig {
        RunConfig {
            program_path: self.root.join("programs/program.toml"),
            run_dir: self.root.join("run"),
            repo_root: self.root.clone(),
        }
    }
    fn transcript_lines(&self) -> usize {
        fs::read_to_string(self.root.join("run/transcript.jsonl"))
            .map(|t| t.lines().count())
            .unwrap_or(0)
    }
}

const TWO_STEP: &str = r#"
name = "toy"
[[step]]
name = "draft"
opcode = "generate"
model = "mock"
template = "draft.md"

[[step]]
name = "critique"
opcode = "generate"
model = "mock"
template = "critique.md"
inputs = ["draft"]
"#;

#[test]
fn vertical_slice_and_resume_skips_done_steps() {
    let fx = Fixture::new(
        "resume",
        TWO_STEP,
        &[("draft.md", "write a haiku"), ("critique.md", "critique this: {{draft}}")],
    );
    let mock = MockTransport::new(vec!["haiku text".into(), "fine haiku".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(*mock.requests_served.borrow(), 2);
    assert_eq!(fx.transcript_lines(), 2);

    // Rerun with an EMPTY mock: completed steps must be loaded, never re-requested.
    let empty = MockTransport::new(vec![]);
    assert_eq!(run(&fx.cfg(), &empty).unwrap(), Outcome::Done);
    assert_eq!(*empty.requests_served.borrow(), 0);
    assert_eq!(fx.transcript_lines(), 2, "resume must not add transcript lines");
}

const VERDICT_STEP: &str = r#"
name = "review"
[[step]]
name = "verdict"
opcode = "generate"
model = "mock"
artifact = "verdict"
retry_cap = 2
template = "review.md"
"#;

#[test]
fn retry_cap_is_hard_then_parks() {
    let fx = Fixture::new("retrycap", VERDICT_STEP, &[("review.md", "review the diff")]);
    // cap 2 => exactly 3 attempts; a 4th canned response must never be requested.
    let mock = MockTransport::new(vec![
        "not json".into(),
        "{\"verdict\": \"maybe\", \"rationale\": \"x\"}".into(),
        "still not json".into(),
        "{\"verdict\": \"accept\", \"rationale\": \"never reached\"}".into(),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(*mock.requests_served.borrow(), 3, "retry cap must stop at cap+1 requests");
    assert_eq!(fx.transcript_lines(), 3, "every request on the record, retries included");
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("\"verdict\""));
}

#[test]
fn parse_feedback_recovers_within_cap() {
    let fx = Fixture::new("recover", VERDICT_STEP, &[("review.md", "review the diff")]);
    let mock = MockTransport::new(vec![
        "garbage".into(),
        "```json\n{\"verdict\": \"accept\", \"rationale\": \"clean\"}\n```".into(),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let artifact = fs::read_to_string(fx.root.join("run/step-00-verdict.json")).unwrap();
    assert!(artifact.contains("\"accept\""));
    assert!(!fx.root.join("run/parked.jsonl").exists());
}

const PARK_BLOCKS_DEPENDENT: &str = r#"
name = "blocked"
[[step]]
name = "gatestep"
opcode = "gate"
gate = ["exit 1"]

[[step]]
name = "unrelated"
opcode = "generate"
model = "mock"
template = "t.md"

[[step]]
name = "dependent"
opcode = "generate"
model = "mock"
template = "dep.md"
inputs = ["gatestep"]
"#;

#[test]
fn red_gate_parks_independent_continues_dependent_blocks() {
    let fx = Fixture::new(
        "block",
        PARK_BLOCKS_DEPENDENT,
        &[("t.md", "carry on"), ("dep.md", "needs {{gatestep}}")],
    );
    let mock = MockTransport::new(vec!["carried on".into()]);
    let outcome = run(&fx.cfg(), &mock).unwrap();
    assert_eq!(outcome, Outcome::Blocked("step \"dependent\" depends on parked step \"gatestep\"".into()));
    // The independent step between them still ran (park never blocks the queue).
    assert_eq!(*mock.requests_served.borrow(), 1);
}

const ESCALATE: &str = r#"
name = "esc"
[[step]]
name = "ask"
opcode = "escalate"
template = "q.md"

[[step]]
name = "use"
opcode = "generate"
model = "mock"
template = "u.md"
inputs = ["ask"]
"#;

#[test]
fn escalate_suspends_then_resumes_on_answer() {
    let fx = Fixture::new("esc", ESCALATE, &[("q.md", "which alpha?"), ("u.md", "use {{ask}}")]);
    let mock = MockTransport::new(vec!["used".into()]);
    let outcome = run(&fx.cfg(), &mock).unwrap();
    let Outcome::Escalated(path) = outcome else {
        panic!("expected escalation, got {outcome:?}")
    };
    assert_eq!(*mock.requests_served.borrow(), 0, "suspends before any model call");

    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\n0.05\n")).unwrap();
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let artifact = fs::read_to_string(fx.root.join("run/step-00-ask.json")).unwrap();
    assert!(artifact.contains("0.05"));
}

#[test]
fn template_slots_are_loud_both_directions() {
    let mut inputs = BTreeMap::new();
    inputs.insert("a".to_string(), "x".to_string());
    assert!(workflow_runtime::template::render("no slot here", &inputs).is_err());
    assert!(workflow_runtime::template::render("{{a}} and {{missing}}", &inputs).is_err());
    assert_eq!(workflow_runtime::template::render("got {{a}}", &inputs).unwrap(), "got x");
}

#[test]
fn ungated_execute_is_rejected() {
    let fx = Fixture::new(
        "exec-nogate",
        "name = \"e\"\n[target]\npath = \"/tmp\"\n[[step]]\nname = \"x\"\nopcode = \"execute\"\nmodel = \"mock\"\ntemplate = \"t.md\"\n",
        &[("t.md", "unused")],
    );
    let mock = MockTransport::new(vec![]);
    let err = run(&fx.cfg(), &mock).unwrap_err();
    assert!(err.contains("no gate"), "{err}");
}

/// A throwaway git repo standing in for the target worktree.
fn init_target_repo(fx: &Fixture) -> PathBuf {
    let dir = fx.root.join("targetrepo");
    fs::create_dir_all(&dir).unwrap();
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?} failed");
    };
    git(&["init", "-q"]);
    git(&["config", "user.email", "t@t"]);
    git(&["config", "user.name", "t"]);
    fs::write(dir.join("lib.rs"), "fn old_name() {}\nfn keep() {}\n").unwrap();
    git(&["add", "--", "lib.rs"]);
    git(&["commit", "-q", "-m", "base", "--", "lib.rs"]);
    dir
}

fn execute_program(target: &PathBuf, retry_cap: u8) -> String {
    format!(
        r#"
name = "exec"
[target]
path = "{}"
[[step]]
name = "rename"
opcode = "execute"
model = "mock"
retry_cap = {retry_cap}
template = "brief.md"
inputs = ["file:lib.rs"]
gate = ["rg -q new_name lib.rs", "! rg -q old_name lib.rs"]
"#,
        target.display()
    )
}

#[test]
fn execute_applies_commits_with_pathspec_and_passes_gate() {
    let fx = Fixture::new("exec-ok", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 2)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();
    // Extra untracked file proves the pathspec: it must NOT enter the commit.
    fs::write(target.join("stray.txt"), "untracked").unwrap();

    let change = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}], "commit_message": "rename"}"#;
    let mock = MockTransport::new(vec![change.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);

    let show = std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "show", "--name-only", "--format=%s", "HEAD"])
        .output()
        .unwrap();
    let show = String::from_utf8_lossy(&show.stdout);
    assert!(show.contains("rename") && show.contains("lib.rs"), "{show}");
    assert!(!show.contains("stray.txt"), "pathspec-only commits: {show}");
}

#[test]
fn execute_red_gate_feeds_back_then_recovers() {
    let fx = Fixture::new("exec-retry", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 2)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    let wrong = r#"{"edits": [{"path": "lib.rs", "find": "fn keep()", "replace": "fn kept()"}], "commit_message": "wrong edit"}"#;
    let right = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}], "commit_message": "right edit"}"#;
    let mock = MockTransport::new(vec![wrong.into(), right.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(*mock.requests_served.borrow(), 2, "gate tail must have been fed back once");
}

#[test]
fn execute_ambiguous_find_feeds_back_without_commit() {
    let fx = Fixture::new("exec-ambig", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(target.join("lib.rs"), "fn old_name() {}\nfn old_name2() {}\n").unwrap();
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 1)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    let ambiguous = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name", "replace": "fn new_name"}], "commit_message": "ambiguous"}"#;
    let mock = MockTransport::new(vec![ambiguous.into(), ambiguous.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // parks, nothing depends on it
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("matches 2 times"), "{parked}");
    let log = std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "log", "--oneline"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&log.stdout).lines().count(), 1, "no commit from a failed apply");
}
