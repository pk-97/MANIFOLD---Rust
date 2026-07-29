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
    assert_eq!(mock.requests_served(), 2);
    assert_eq!(fx.transcript_lines(), 2);

    // Rerun with an EMPTY mock: completed steps must be loaded, never re-requested.
    let empty = MockTransport::new(vec![]);
    assert_eq!(run(&fx.cfg(), &empty).unwrap(), Outcome::Done);
    assert_eq!(empty.requests_served(), 0);
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
    assert_eq!(mock.requests_served(), 3, "retry cap must stop at cap+1 requests");
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
    assert_eq!(mock.requests_served(), 1);
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
    assert_eq!(mock.requests_served(), 0, "suspends before any model call");

    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{text}\n0.05\n")).unwrap();
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let artifact = fs::read_to_string(fx.root.join("run/step-00-ask.json")).unwrap();
    assert!(artifact.contains("0.05"));
}

#[test]
fn token_budget_suspends_before_the_next_call() {
    let fx = Fixture::new(
        "budget",
        "name = \"b\"\ntoken_budget = 5\n[[step]]\nname = \"a\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"t.md\"\n[[step]]\nname = \"b\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"t.md\"\n",
        &[("t.md", "go")],
    );
    // First call reports 8 tokens (> cap 5): the second call must never fire.
    let mock = MockTransport::with_tokens_per_response(vec!["one".into(), "two".into()], 8);
    let err = run(&fx.cfg(), &mock).unwrap_err();
    assert!(err.contains("token budget exhausted (8/5)"), "{err}");
    assert_eq!(mock.requests_served(), 1);

    // Resume after a raise: spend is re-read from the transcript, step a is kept.
    let raised = fs::read_to_string(fx.root.join("programs/program.toml")).unwrap().replace("token_budget = 5", "token_budget = 100");
    fs::write(fx.root.join("programs/program.toml"), raised).unwrap();
    let mock2 = MockTransport::with_tokens_per_response(vec!["two".into()], 8);
    assert_eq!(run(&fx.cfg(), &mock2).unwrap(), Outcome::Done);
    assert_eq!(mock2.requests_served(), 1, "resume keeps completed steps");
}

#[test]
fn escalation_question_quoting_the_marker_cannot_self_answer() {
    // Finding 1: a question that MENTIONS the answer marker must still wait.
    let fx = Fixture::new(
        "esc-marker",
        ESCALATE,
        &[("q.md", "Write below '## ANSWER (write below this line, then rerun)' please: which alpha?"), ("u.md", "use {{ask}}")],
    );
    let mock = MockTransport::new(vec!["used".into()]);
    let Outcome::Escalated(path) = run(&fx.cfg(), &mock).unwrap() else { panic!() };
    // Rerun with NO answer written — must stay escalated, never self-answer.
    let outcome = run(&fx.cfg(), &mock).unwrap();
    assert_eq!(outcome, Outcome::Escalated(path.clone()));
    fs::write(&path, format!("{}\n0.07\n", fs::read_to_string(&path).unwrap())).unwrap();
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
}

#[test]
fn unpark_lets_a_rerun_retry() {
    // Finding 2: parked must be recoverable through the CLI-sanctioned path.
    let fx = Fixture::new(
        "unpark",
        "name = \"u\"\n[[step]]\nname = \"g\"\nopcode = \"gate\"\ngate = [\"test -f flag\"]",
        &[],
    );
    let mock = MockTransport::new(vec![]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // parks g
    assert!(fx.root.join("run/parked.jsonl").exists());
    fs::write(fx.root.join("flag"), "x").unwrap(); // fix the environment
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // still parked: skipped
    assert!(fx.root.join("run/parked.jsonl").exists());
    workflow_runtime::runner::unpark(&fx.root.join("run"), "g").unwrap();
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert!(fx.root.join("run/step-00-g.json").exists(), "gate retried and passed");
}

#[test]
fn changed_step_list_refuses_resume_but_budget_raise_is_fine() {
    // Finding 7: reordered/renamed steps refuse; a token_budget raise resumes.
    let fx = Fixture::new("progchange", TWO_STEP, &[("draft.md", "d"), ("critique.md", "c {{draft}}")]);
    let mock = MockTransport::new(vec!["a".into(), "b".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let renamed = TWO_STEP.replace("draft", "sketch"); // rename ripples through inputs
    fs::write(fx.root.join("programs/program.toml"), renamed).unwrap();
    let err = run(&fx.cfg(), &MockTransport::new(vec![])).unwrap_err();
    assert!(err.contains("step list changed"), "{err}");
}

#[test]
fn edit_and_write_to_same_path_is_refused() {
    // Finding 12: a write would silently clobber the edit.
    let fx = Fixture::new("overlap", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 0)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();
    let overlap = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}],
        "writes": [{"path": "lib.rs", "content": "fn something_else() {}"}], "commit_message": "overlap"}"#;
    let mock = MockTransport::new(vec![overlap.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // parks
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("both `edits` and `writes`"), "{parked}");
    assert!(fs::read_to_string(target.join("lib.rs")).unwrap().contains("fn old_name()"));
}

#[test]
fn torn_trailing_transcript_line_is_tolerated() {
    // Finding 11: a kill mid-append must not brick the resume.
    let fx = Fixture::new("torn", TWO_STEP, &[("draft.md", "d"), ("critique.md", "c {{draft}}")]);
    let mock = MockTransport::new(vec!["a".into(), "b".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let tpath = fx.root.join("run/transcript.jsonl");
    let mut t = fs::read_to_string(&tpath).unwrap();
    t.push_str("{\"step\": \"torn mid-wri"); // simulated kill mid-append
    fs::write(&tpath, t).unwrap();
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![])).unwrap(), Outcome::Done);
}

#[test]
fn gate_timeout_kills_and_fails() {
    // Finding 5: a hung gate dies, marked TIMEOUT.
    let fx = Fixture::new(
        "gatehang",
        "name = \"h\"\n[[step]]\nname = \"hang\"\nopcode = \"gate\"\ngate = [\"sleep 30\"]\ngate_timeout_s = 1",
        &[],
    );
    let start = std::time::Instant::now();
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![])).unwrap(), Outcome::Done); // parks
    assert!(start.elapsed().as_secs() < 10, "gate must be killed at the timeout");
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("TIMEOUT"), "{parked}");
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

fn execute_program(target: &std::path::Path, retry_cap: u8) -> String {
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
gate = ["grep -q new_name lib.rs", "! grep -q old_name lib.rs"]
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
    // A red gate ALSO commits, so the git asserts below can't tell — this can
    // (the rg-gates-exit-127 blind spot, found 2026-07-30).
    assert!(!fx.root.join("run/parked.jsonl").exists(), "gate must actually pass, not park");

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
    assert_eq!(mock.requests_served(), 2, "gate tail must have been fed back once");
}

#[test]
fn failed_apply_leaves_tree_untouched() {
    let fx = Fixture::new("exec-atomic", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 0)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    // First edit is valid, second is not — the first must NOT land on disk.
    let half_bad = r#"{"edits": [
        {"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"},
        {"path": "lib.rs", "find": "fn does_not_exist()", "replace": "x"}],
        "commit_message": "half bad"}"#;
    let mock = MockTransport::new(vec![half_bad.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // parks
    let lib = fs::read_to_string(target.join("lib.rs")).unwrap();
    assert!(lib.contains("fn old_name()"), "atomic apply: no partial writes, got: {lib}");
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

// ── v1.1: transform / fanout / sample / parallel generate / anchors / scrub ──

#[test]
fn secret_in_context_aborts_before_any_call() {
    let fx = Fixture::new(
        "scrub",
        TWO_STEP,
        &[
            ("draft.md", "summarize: sk-abcdefghijklmnopqrstuvwxyz123456"),
            ("critique.md", "critique this: {{draft}}"),
        ],
    );
    let mock = MockTransport::new(vec!["never sent".into()]);
    let err = run(&fx.cfg(), &mock).unwrap_err();
    assert!(err.contains("secret-shaped"), "{err}");
    assert!(!err.contains("z123456"), "the secret must be masked: {err}");
    assert_eq!(mock.requests_served(), 0, "nothing ships once a secret is seen");
    assert_eq!(fx.transcript_lines(), 0);
}

const TRANSFORM: &str = r#"
name = "tf"
[[step]]
name = "draft"
opcode = "generate"
model = "mock"
template = "draft.md"

[[step]]
name = "upper"
opcode = "transform"
command = "tr 'a-z' 'A-Z'"
template = "pass.md"
inputs = ["draft"]
"#;

#[test]
fn transform_reshapes_deterministically_no_model() {
    let fx = Fixture::new("transform", TRANSFORM, &[("draft.md", "say hi"), ("pass.md", "{{draft}}")]);
    let mock = MockTransport::new(vec!["hello".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 1, "transform must make no model call");
    let artifact = fs::read_to_string(fx.root.join("run/step-01-upper.json")).unwrap();
    assert!(artifact.contains("HELLO"), "{artifact}");
}

#[test]
fn transform_nonzero_exit_parks_without_retry() {
    let program = TRANSFORM.replace("tr 'a-z' 'A-Z'", "exit 3");
    let fx = Fixture::new("transform-red", &program, &[("draft.md", "say hi"), ("pass.md", "{{draft}}")]);
    let mock = MockTransport::new(vec!["hello".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("transform exited 3"), "{parked}");
}

const FANOUT: &str = r#"
name = "fan"
[[step]]
name = "list"
opcode = "generate"
model = "mock"
artifact = "json"
template = "list.md"

[[step]]
name = "expand"
opcode = "fanout"
model = "mock"
over = "list"
template = "each.md"
"#;

#[test]
fn fanout_runs_template_per_element_and_collects() {
    let fx = Fixture::new("fanout", FANOUT, &[("list.md", "list three"), ("each.md", "expand {{item}}")]);
    let mock = MockTransport::new(vec![
        r#"["a", "b", "c"]"#.into(),
        "A!".into(),
        "B!".into(),
        "C!".into(),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 4);
    let artifact: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(fx.root.join("run/step-01-expand.json")).unwrap()).unwrap();
    assert_eq!(artifact["value"], serde_json::json!(["A!", "B!", "C!"]));
}

#[test]
fn fanout_element_past_cap_parks_whole_step() {
    let program = FANOUT.replace("template = \"each.md\"", "template = \"each.md\"\nartifact = \"json\"\nretry_cap = 0");
    let fx = Fixture::new("fanout-park", &program, &[("list.md", "list"), ("each.md", "expand {{item}}")]);
    let mock = MockTransport::new(vec![r#"["a", "b"]"#.into(), "42".into(), "not json".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("element 1 of 2 parked"), "partial collections are not artifacts: {parked}");
    assert!(!fx.root.join("run/step-01-expand.json").exists());
}

const SAMPLE_GATE: &str = r#"
name = "sample-gate"
[[step]]
name = "pick"
opcode = "sample"
model = "mock"
samples = 3
template = "t.md"
gate = ["grep -q GOOD \"$WORKFLOW_SAMPLE\""]
"#;

#[test]
fn sample_gate_picks_first_passing_candidate() {
    let fx = Fixture::new("sample", SAMPLE_GATE, &[("t.md", "try")]);
    let mock = MockTransport::new(vec!["bad one".into(), "GOOD two".into(), "GOOD three (never wins)".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 3, "all k samples are drawn before selection");
    let artifact = fs::read_to_string(fx.root.join("run/step-00-pick.json")).unwrap();
    assert!(artifact.contains("GOOD two"), "first pass wins deterministically: {artifact}");
}

#[test]
fn sample_no_winner_parks() {
    let fx = Fixture::new("sample-none", SAMPLE_GATE, &[("t.md", "try")]);
    let mock = MockTransport::new(vec!["bad".into(), "worse".into(), "nope".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("passed the gate"), "{parked}");
}

const SAMPLE_VOTE: &str = r#"
name = "sample-vote"
[[step]]
name = "vote"
opcode = "sample"
model = "mock"
samples = 3
artifact = "verdict"
template = "t.md"
"#;

#[test]
fn sample_verdict_majority_wins_tie_parks() {
    let accept = r#"{"verdict": "accept", "rationale": "fine"}"#;
    let reject = r#"{"verdict": "reject", "rationale": "off"}"#;
    let fx = Fixture::new("vote", SAMPLE_VOTE, &[("t.md", "judge")]);
    let mock = MockTransport::new(vec![accept.into(), reject.into(), accept.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let artifact = fs::read_to_string(fx.root.join("run/step-00-vote.json")).unwrap();
    assert!(artifact.contains("accept"), "{artifact}");

    // 2 samples, 1-1: a tie is a park, never a model tiebreak.
    let tied = SAMPLE_VOTE.replace("samples = 3", "samples = 2").replace("sample-vote", "sample-tie");
    let fx2 = Fixture::new("vote-tie", &tied, &[("t.md", "judge")]);
    let mock2 = MockTransport::new(vec![accept.into(), reject.into()]);
    assert_eq!(run(&fx2.cfg(), &mock2).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx2.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("tied 1-1"), "{parked}");
}

const PARALLEL: &str = r#"
name = "par"
parallel = true
[[step]]
name = "alpha"
opcode = "generate"
model = "mock"
template = "alpha.md"

[[step]]
name = "beta"
opcode = "generate"
model = "mock"
template = "beta.md"

[[step]]
name = "join"
opcode = "generate"
model = "mock"
template = "join.md"
inputs = ["alpha", "beta"]
"#;

#[test]
fn parallel_generates_run_and_join_sees_both() {
    let fx = Fixture::new(
        "parallel",
        PARALLEL,
        &[("alpha.md", "make alpha"), ("beta.md", "make beta"), ("join.md", "join {{alpha}} {{beta}}")],
    );
    // Keyed mock: pop order is racy across threads, needles are not.
    let mock = MockTransport::keyed(vec![
        ("make alpha".into(), "A-out".into()),
        ("make beta".into(), "B-out".into()),
        ("join A-out B-out".into(), "joined".into()),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 3);
    assert_eq!(fx.transcript_lines(), 3, "parallel attempts are logged after the join");
    // The join step (dependent) must run AFTER the batch, sequentially.
    let joined = fs::read_to_string(fx.root.join("run/step-02-join.json")).unwrap();
    assert!(joined.contains("joined"), "{joined}");
}

#[test]
fn anchor_input_resolves_span_and_missing_anchor_parks() {
    let fx = Fixture::new(
        "anchor",
        "name = \"anch\"\n[[step]]\nname = \"read\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"t.md\"\ninputs = [\"anchor:target_fn\"]\n",
        &[("t.md", "explain:\n{{anchor:target_fn}}")],
    );
    fs::write(fx.root.join("code.rs"), "pub fn target_fn() -> u32 {\n    7\n}\nfn other() {}\n").unwrap();
    let mock = MockTransport::new(vec!["explained".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    // The model saw the SPAN, not the whole file.
    let transcript = fs::read_to_string(fx.root.join("run/transcript.jsonl")).unwrap();
    assert!(transcript.contains("code.rs:1-3"), "{transcript}");
    assert!(!transcript.contains("fn other"), "span-level, not file-level: {transcript}");

    // Missing anchor: deterministic park, zero model calls.
    let fx2 = Fixture::new(
        "anchor-miss",
        "name = \"anch2\"\n[[step]]\nname = \"read\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"t.md\"\ninputs = [\"anchor:gone_fn\"]\n",
        &[("t.md", "explain:\n{{anchor:gone_fn}}")],
    );
    let mock2 = MockTransport::new(vec!["never".into()]);
    assert_eq!(run(&fx2.cfg(), &mock2).unwrap(), Outcome::Done);
    assert_eq!(mock2.requests_served(), 0);
    let parked = fs::read_to_string(fx2.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("resolves to nothing"), "{parked}");
}

#[test]
fn check_linter_collects_all_findings() {
    let fx = Fixture::new(
        "lint",
        "name = \"lint\"\n[[step]]\nname = \"a\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"missing.md\"\n[[step]]\nname = \"b\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"b.md\"\ninputs = [\"file:no/such/file.rs\", \"a\"]\n",
        &[("b.md", "uses {{a}} but not the file input")],
    );
    let findings = workflow_runtime::check::check(&fx.root.join("programs/program.toml"), &fx.root);
    assert_eq!(findings.len(), 3, "template miss + unused input + missing file: {findings:?}");

    let ok = Fixture::new(
        "lint-ok",
        "name = \"lintok\"\n[[step]]\nname = \"a\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"a.md\"\n",
        &[("a.md", "no inputs")],
    );
    assert!(workflow_runtime::check::check(&ok.root.join("programs/program.toml"), &ok.root).is_empty());
}

#[test]
fn cost_ledger_sums_transcript() {
    let fx = Fixture::new("cost", TWO_STEP, &[("draft.md", "d"), ("critique.md", "c {{draft}}")]);
    let mock = MockTransport::with_tokens_per_response(vec!["a".into(), "b".into()], 11);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let report = workflow_runtime::cost::summarize(&fx.root.join("run")).unwrap();
    assert!(report.contains("TOTAL 22 tokens"), "{report}");
    assert!(report.contains("draft") && report.contains("critique"), "{report}");
}
