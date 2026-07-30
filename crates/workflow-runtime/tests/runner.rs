//! P1 gates (WORKFLOW_RUNTIME_DESIGN.md section 5, Phasing): retry cap, resume,
//! escalate, transcript completeness, park-vs-block — against the mock seam.

use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use workflow_runtime::lane::{LaneOutcome, LaneRequest, LaneWorker};
use workflow_runtime::runner::{Outcome, RunConfig, run, run_with};
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

const ONE_STEP: &str = r#"
name = "onestep"
[[step]]
name = "draft"
opcode = "generate"
model = "mock"
template = "draft.md"
"#;

#[test]
fn status_json_tracks_run_to_done() {
    let fx = Fixture::new("status", ONE_STEP, &[("draft.md", "write a haiku")]);
    let mock = MockTransport::new(vec!["haiku text".into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let st = workflow_runtime::status::read(&fx.root.join("run")).expect("status.json readable");
    assert_eq!(st.state, "run-done");
    assert_eq!(st.total_steps, 1);
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
        "```json\n{\"verdict\": \"accept\", \"rationale\": \"clean diff, matches the brief exactly\"}\n```".into(),
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
    workflow_runtime::runner::unpark(&fx.root.join("run"), "g", "the flag file exists now").unwrap();
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
title = "Rename old_name to new_name"
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
    let accept = r#"{"verdict": "accept", "rationale": "meets the brief and gates are green"}"#;
    let reject = r#"{"verdict": "reject", "rationale": "scope creep beyond the named files"}"#;
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

// ── D8: verdict recording through gate_runner review ──

/// Plant a stub `scripts/gate_runner.py` that appends its argv to a file.
fn plant_gate_runner_stub(fx: &Fixture) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;
    let log = fx.root.join("review-calls.log");
    fs::create_dir_all(fx.root.join("scripts")).unwrap();
    let stub = fx.root.join("scripts/gate_runner.py");
    fs::write(&stub, format!("#!/bin/sh\necho \"$@\" >> '{}'\n", log.display())).unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
    log
}

const VERDICT_WITH_TASK: &str = r#"
name = "reviewed"
task = "BUG-test"
[[step]]
name = "verdict"
opcode = "generate"
model = "mock"
artifact = "verdict"
template = "review.md"
"#;

#[test]
fn verdict_step_with_task_is_recorded_via_gate_runner() {
    let fx = Fixture::new("d8", VERDICT_WITH_TASK, &[("review.md", "review the diff")]);
    let log = plant_gate_runner_stub(&fx);
    let mock = MockTransport::new(vec![
        r#"{"verdict": "accept", "rationale": "meets the brief and gates are green"}"#.into(),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    let calls = fs::read_to_string(&log).unwrap();
    assert!(calls.contains("review"), "{calls}");
    assert!(calls.contains("--task BUG-test"), "{calls}");
    assert!(calls.contains("--verdict accept"), "{calls}");
    assert!(calls.contains("--by mock"), "the MODEL is the reviewing seat: {calls}");

    // Resume must not double-record: the step loads from disk, no new call.
    let empty = MockTransport::new(vec![]);
    assert_eq!(run(&fx.cfg(), &empty).unwrap(), Outcome::Done);
    assert_eq!(fs::read_to_string(&log).unwrap().lines().count(), 1, "one verdict, one record");
}

#[test]
fn verdict_without_task_stays_in_run_dir_and_short_rationale_feeds_back() {
    // No `task` field: the shared trail is never touched.
    let fx = Fixture::new("d8-notask", VERDICT_STEP, &[("review.md", "review the diff")]);
    let log = plant_gate_runner_stub(&fx);
    let mock = MockTransport::new(vec![
        // Short rationale: parse failure fed back (mirrors gate_runner's floor).
        r#"{"verdict": "accept", "rationale": "lgtm"}"#.into(),
        r#"{"verdict": "accept", "rationale": "meets the brief and gates are green"}"#.into(),
    ]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 2, "short rationale must cost a retry");
    assert!(!log.exists(), "no task, no write to the shared trail");
}

#[test]
fn failed_recording_is_a_hard_error() {
    // A verdict that cannot be recorded must never look green: the fixture
    // has NO gate_runner stub, so the spawn fails and the run aborts.
    let fx = Fixture::new("d8-hard", VERDICT_WITH_TASK, &[("review.md", "review the diff")]);
    let mock = MockTransport::new(vec![
        r#"{"verdict": "accept", "rationale": "meets the brief and gates are green"}"#.into(),
    ]);
    let err = run(&fx.cfg(), &mock).unwrap_err();
    assert!(err.contains("gate_runner review"), "{err}");
}

// ── P3 shakedown flow fixes: serial executes, empty ChangeSet, unpark seed ──

#[test]
fn parked_execute_blocks_every_later_execute() {
    // Execute steps share one worktree: a parked execute blocks later
    // executes even with no `inputs` edge between them (exit 20).
    let fx = Fixture::new(
        "exec-serial",
        "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]",
        &[],
    );
    let target = init_target_repo(&fx);
    let program = format!(
        r#"
name = "exec-serial"
[target]
path = "{}"
[[step]]
name = "first"
opcode = "execute"
model = "mock"
retry_cap = 0
template = "brief.md"
inputs = ["file:lib.rs"]
gate = ["true"]

[[step]]
name = "second"
opcode = "execute"
model = "mock"
retry_cap = 0
template = "brief.md"
inputs = ["file:lib.rs"]
gate = ["true"]
"#,
        target.display()
    );
    fs::write(fx.root.join("programs/program.toml"), program).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "edit:\n{{file:lib.rs}}").unwrap();
    // First step's only attempt fails to parse — it parks.
    let mock = MockTransport::new(vec!["not a changeset".into(), "never requested".into()]);
    let outcome = run(&fx.cfg(), &mock).unwrap();
    let Outcome::Blocked(reason) = outcome else { panic!("expected blocked, got {outcome:?}") };
    assert!(reason.contains("\"second\"") && reason.contains("\"first\""), "{reason}");
    assert_eq!(mock.requests_served(), 1, "the second execute must never call the model");
}

#[test]
fn empty_changeset_is_a_non_attempt_and_keeps_the_informative_error() {
    let fx = Fixture::new(
        "exec-empty",
        "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]",
        &[],
    );
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 2)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    // Attempt 1 applies and commits but the gate is red; 2 and 3 are empty.
    let wrong = r#"{"edits": [{"path": "lib.rs", "find": "fn keep()", "replace": "fn kept()"}], "commit_message": "wrong edit"}"#;
    let empty = r#"{"edits": [], "writes": [], "commit_message": "empty"}"#;
    let mock = MockTransport::new(vec![wrong.into(), empty.into(), empty.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done); // parks, nothing depends on it
    assert_eq!(mock.requests_served(), 3, "an empty ChangeSet still costs an attempt");

    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("gate is red"), "park reason must keep the red gate: {parked}");
    assert!(!parked.contains("\"reason\":\"ChangeSet has no edits"), "the empty set must never be the reason: {parked}");
    assert!(parked.contains("\"gate_report\""), "full gate report preserved in the park record: {parked}");
    assert!(parked.contains("Rename old_name to new_name"), "the step title rides along: {parked}");

    // The final attempt's prompt carried BOTH the real error and the empty-set note.
    let transcript = fs::read_to_string(fx.root.join("run/transcript.jsonl")).unwrap();
    let last = transcript.lines().last().unwrap();
    assert!(last.contains("EMPTY ChangeSet"), "{last}");
    assert!(last.contains("gate is red"), "the real error must stay in front of the model: {last}");
}

/// D19 (gate-first on a seeded rerun): a previously parked execute's rerun
/// checks the gate BEFORE the first model call, not after — the seed's own
/// worktree state decides which branch runs, never a blind model call first.
#[test]
fn unpark_gate_still_red_feeds_the_fresh_report_not_the_stale_seed() {
    let fx = Fixture::new(
        "exec-seed-red",
        "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]",
        &[],
    );
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 0)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    // Single attempt with a misquoted `find` — apply fails, nothing commits,
    // the worktree is unchanged from base, so the rerun's gate stays red.
    let bad = r#"{"edits": [{"path": "lib.rs", "find": "fn misquoted()", "replace": "x"}], "commit_message": "bad find"}"#;
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![bad.into()])).unwrap(), Outcome::Done);
    assert!(fx.root.join("run/parked.jsonl").exists());

    workflow_runtime::runner::unpark(&fx.root.join("run"), "rename", "quote the file exactly this time").unwrap();
    let right = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}], "commit_message": "fixed"}"#;
    let mock = MockTransport::new(vec![right.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 1, "the pre-flight gate check costs no model call");
    assert!(!fx.root.join("run/parked.jsonl").exists(), "the rerun must succeed and clear the park");

    // The rerun's FIRST prompt carried the FRESH gate report (gate still red
    // on the unchanged worktree), not the stale "misquoted" park text.
    let transcript = fs::read_to_string(fx.root.join("run/transcript.jsonl")).unwrap();
    let last = transcript.lines().last().unwrap();
    assert!(last.contains("Work already committed in the worktree stands"), "{last}");
    assert!(last.contains("The gate is currently red"), "{last}");
    assert!(last.contains("\\\"pass\\\": false"), "the fresh gate report rides along: {last}");
    assert!(!last.contains("misquoted"), "the stale seed text must be discarded: {last}");
}

/// D19 (gate-first on a seeded rerun): if the worktree is already complete
/// when the pre-flight gate runs, the step finishes with ZERO model calls.
#[test]
fn unpark_gate_already_green_completes_with_zero_model_calls() {
    let fx = Fixture::new(
        "exec-seed-green",
        "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]",
        &[],
    );
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 0)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();

    // The edit lands and commits, but the gate is red for an UNRELATED reason
    // this sample (simulated here as a bad edit that still commits).
    let wrong = r#"{"edits": [{"path": "lib.rs", "find": "fn keep()", "replace": "fn kept()"}], "commit_message": "wrong edit"}"#;
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![wrong.into()])).unwrap(), Outcome::Done);
    assert!(fx.root.join("run/parked.jsonl").exists());

    // Out-of-band, the worktree gets fixed forward to a state that DOES pass
    // the gate (a human, or a later sample's own committed edit landed here).
    fs::write(target.join("lib.rs"), "fn new_name() {}\nfn kept() {}\n").unwrap();
    std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "add", "--", "lib.rs"])
        .output()
        .unwrap();
    std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "commit", "-q", "-m", "fixed out of band", "--", "lib.rs"])
        .output()
        .unwrap();

    workflow_runtime::runner::unpark(&fx.root.join("run"), "rename", "quote the file exactly this time").unwrap();
    // Zero-length mock: any model call at all is a hard failure.
    let mock = MockTransport::new(vec![]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 0, "gate already green — no model call needed");
    assert!(!fx.root.join("run/parked.jsonl").exists(), "the rerun must complete and clear the park");

    let state = fs::read_to_string(fx.root.join("run/step-00-rename.json")).unwrap();
    assert!(state.contains("already_complete"), "{state}");
}

// ── P4: lane opcode, promote-on-first-substantive-failure, USD cap, ledger ──

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} failed");
}

/// What a fake lane does before it answers. The live worker is an agent
/// session; a test must never launch one, so the seam takes a double.
enum LaneAction {
    /// Write `path` and COMMIT it — a lane commits its own work by doctrine,
    /// so this is the well-behaved shape.
    Commit(&'static str, &'static str),
    /// Write without committing: a protocol violation the runtime must park.
    LeaveDirty(&'static str, &'static str),
    /// Answer having touched nothing: HEAD unmoved, the lane's empty ChangeSet.
    NoChange,
    /// The worker itself failed (timeout, budget cap, unknown provider).
    Fail(&'static str),
}

struct FakeLane {
    actions: Mutex<VecDeque<LaneAction>>,
    launches: Mutex<u32>,
    prompts: Mutex<Vec<String>>,
}

impl FakeLane {
    fn new(actions: Vec<LaneAction>) -> FakeLane {
        FakeLane { actions: Mutex::new(actions.into()), launches: Mutex::new(0), prompts: Mutex::new(Vec::new()) }
    }
    fn launches(&self) -> u32 {
        *self.launches.lock().unwrap()
    }
    fn prompt(&self, i: usize) -> String {
        self.prompts.lock().unwrap()[i].clone()
    }
}

impl LaneWorker for FakeLane {
    fn run(&self, req: &LaneRequest) -> Result<LaneOutcome, String> {
        self.prompts.lock().unwrap().push(req.prompt.clone());
        *self.launches.lock().unwrap() += 1;
        let action = self.actions.lock().unwrap().pop_front();
        let Some(action) = action else {
            return Err("fake lane exhausted — an unexpected launch".to_string());
        };
        let envelope = serde_json::json!({"ok": true, "result": "did the work", "total_cost_usd": 0.25});
        let ok = |e: serde_json::Value| Ok(LaneOutcome { ok: true, envelope: e, usd: 0.25, error: String::new() });
        match action {
            LaneAction::Commit(path, content) => {
                fs::write(req.worktree.join(path), content).unwrap();
                git(&req.worktree, &["add", "--", path]);
                git(&req.worktree, &["commit", "-q", "-m", "lane work", "--", path]);
                ok(envelope)
            }
            LaneAction::LeaveDirty(path, content) => {
                fs::write(req.worktree.join(path), content).unwrap();
                ok(envelope)
            }
            LaneAction::NoChange => ok(envelope),
            LaneAction::Fail(msg) => Ok(LaneOutcome {
                ok: false,
                envelope: serde_json::json!({"ok": false, "error_code": "SUBAGENT_FAILED", "error_msg": msg}),
                usd: 0.1,
                error: format!("lane worker failed: {msg}"),
            }),
        }
    }
}

/// The renamed file the gate wants, and a version that still fails it.
const RENAMED: &str = "fn new_name() {}\nfn keep() {}\n";
const STILL_WRONG: &str = "fn old_name() {}\nfn kept() {}\n";

fn lane_program(target: &std::path::Path, retry_cap: u8) -> String {
    format!(
        r#"
name = "lane"
[target]
path = "{}"
[[step]]
name = "rename"
title = "Rename old_name to new_name"
opcode = "lane"
model = "sonnet"
retry_cap = {retry_cap}
template = "brief.md"
inputs = ["path:lib.rs"]
gate = ["grep -q new_name lib.rs", "! grep -q old_name lib.rs"]
"#,
        target.display()
    )
}

fn lane_fixture(name: &str, retry_cap: u8) -> (Fixture, PathBuf) {
    let fx = Fixture::new(name, "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), lane_program(&target, retry_cap)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in {{path:lib.rs}}").unwrap();
    (fx, target)
}

#[test]
fn lane_commits_its_own_work_and_the_head_delta_is_the_oracle() {
    let (fx, target) = lane_fixture("lane-ok", 2);
    let before = head(&target);
    let lane = FakeLane::new(vec![LaneAction::Commit("lib.rs", RENAMED)]);
    assert_eq!(run_with(&fx.cfg(), &MockTransport::new(vec![]), &lane).unwrap(), Outcome::Done);
    assert_eq!(lane.launches(), 1);
    assert!(!fx.root.join("run/parked.jsonl").exists(), "the gate must actually pass");
    assert_ne!(head(&target), before, "the lane's own commit is the work");

    let state = fs::read_to_string(fx.root.join("run/step-00-rename.json")).unwrap();
    assert!(state.contains("did the work"), "the raw envelope is recorded: {state}");
    assert!(state.contains("lib.rs"), "the files come from the sha delta: {state}");
    // The brief named the file by PATH — it must not have pasted the contents.
    assert!(lane.prompt(0).contains("lib.rs"), "{}", lane.prompt(0));
    assert!(!lane.prompt(0).contains("fn old_name"), "path: pastes the path, never the file: {}", lane.prompt(0));

    let cost = workflow_runtime::cost::summarize(&fx.root.join("run")).unwrap();
    assert!(cost.contains("TOTAL 0 tokens") && cost.contains("$0.2500 lane spend"), "{cost}");
}

#[test]
fn lane_red_gate_feeds_back_then_green_on_retry() {
    let (fx, _t) = lane_fixture("lane-retry", 2);
    let lane =
        FakeLane::new(vec![LaneAction::Commit("lib.rs", STILL_WRONG), LaneAction::Commit("lib.rs", RENAMED)]);
    assert_eq!(run_with(&fx.cfg(), &MockTransport::new(vec![]), &lane).unwrap(), Outcome::Done);
    assert_eq!(lane.launches(), 2);
    assert!(lane.prompt(1).contains("the gate is red"), "{}", lane.prompt(1));
    assert!(!fx.root.join("run/parked.jsonl").exists());
}

#[test]
fn lane_leaving_the_worktree_dirty_parks_instead_of_committing_for_it() {
    let (fx, target) = lane_fixture("lane-dirty", 0);
    let before = head(&target);
    // The work is correct but uncommitted. Auto-committing it would be
    // `add -A` in disguise; the runtime must refuse and say so.
    let lane = FakeLane::new(vec![LaneAction::LeaveDirty("lib.rs", RENAMED)]);
    assert_eq!(run_with(&fx.cfg(), &MockTransport::new(vec![]), &lane).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("uncommitted worktree") && parked.contains("lib.rs"), "{parked}");
    assert_eq!(head(&target), before, "the runtime must not commit on the lane's behalf");
}

#[test]
fn lane_same_head_is_a_non_attempt_and_the_cap_parks_with_the_gate_report() {
    let (fx, _t) = lane_fixture("lane-nochange", 2);
    // Attempt 1 commits real work and the gate goes red; 2 and 3 do nothing.
    let lane = FakeLane::new(vec![
        LaneAction::Commit("lib.rs", STILL_WRONG),
        LaneAction::NoChange,
        LaneAction::NoChange,
    ]);
    assert_eq!(run_with(&fx.cfg(), &MockTransport::new(vec![]), &lane).unwrap(), Outcome::Done);
    assert_eq!(lane.launches(), 3, "a no-change run still costs an attempt");

    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("gate is red"), "the informative error survives: {parked}");
    assert!(parked.contains("\"gate_report\""), "{parked}");
    let last = lane.prompt(2);
    assert!(last.contains("left the worktree completely unchanged"), "{last}");
    assert!(last.contains("the gate is red"), "the real error stays in front of it: {last}");
}

#[test]
fn parked_lane_blocks_a_later_execute() {
    // D15 widened: they share the one worktree, so a parked lane must stop a
    // later execute from building on a broken tree (exit 20).
    let fx = Fixture::new("lane-blocks", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    let program = format!(
        r#"
name = "lane-blocks"
[target]
path = "{}"
[[step]]
name = "first"
opcode = "lane"
model = "sonnet"
retry_cap = 0
template = "brief.md"
inputs = ["path:lib.rs"]
gate = ["grep -q new_name lib.rs"]

[[step]]
name = "second"
opcode = "execute"
model = "mock"
retry_cap = 0
template = "brief.md"
inputs = ["path:lib.rs"]
gate = ["true"]
"#,
        target.display()
    );
    fs::write(fx.root.join("programs/program.toml"), program).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "edit {{path:lib.rs}}").unwrap();
    let lane = FakeLane::new(vec![LaneAction::NoChange]);
    let mock = MockTransport::new(vec!["never requested".into()]);
    let outcome = run_with(&fx.cfg(), &mock, &lane).unwrap();
    let Outcome::Blocked(reason) = outcome else { panic!("expected blocked, got {outcome:?}") };
    assert!(reason.contains("\"second\"") && reason.contains("\"first\""), "{reason}");
    assert_eq!(mock.requests_served(), 0, "the execute must never call the model");
}

fn head(target: &std::path::Path) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "rev-parse", "HEAD"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn promote_program(target: &std::path::Path, retry_cap: u8, usd_budget: &str) -> String {
    format!(
        r#"
name = "promote"
{usd_budget}
[target]
path = "{}"
[[step]]
name = "rename"
title = "Rename old_name to new_name"
opcode = "execute"
model = "mock"
lane_model = "sonnet"
on_fail = "lane"
retry_cap = {retry_cap}
template = "brief.md"
inputs = ["file:lib.rs"]
gate = ["grep -q new_name lib.rs", "! grep -q old_name lib.rs"]
"#,
        target.display()
    )
}

fn promote_fixture(name: &str, retry_cap: u8, usd_budget: &str) -> (Fixture, PathBuf) {
    let fx = Fixture::new(name, "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), promote_program(&target, retry_cap, usd_budget)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();
    (fx, target)
}

/// A ChangeSet that commits cleanly but leaves the gate red.
const WRONG_EDIT: &str =
    r#"{"edits": [{"path": "lib.rs", "find": "fn keep()", "replace": "fn kept()"}]}"#;
/// A `find` the model invented — nothing is applied, nothing is committed.
const FIND_MISS: &str =
    r#"{"edits": [{"path": "lib.rs", "find": "fn misquoted()", "replace": "x"}]}"#;
const EMPTY: &str = r#"{"edits": [], "writes": []}"#;
/// A whole-file rewrite of a file that already exists — lane work now.
const REWRITE: &str = r#"{"writes": [{"path": "lib.rs", "content": "fn new_name() {}\n"}]}"#;

#[test]
fn every_substantive_failure_promotes_on_the_first_attempt() {
    // Four kinds, four runs: each must hand over after exactly ONE model call.
    for (name, response, needle) in [
        ("gate-red", WRONG_EDIT, "gate is red"),
        ("find-miss", FIND_MISS, "not in the file"),
        ("empty", EMPTY, "no edits and no writes"),
        ("rewrite", REWRITE, "NEW files only"),
    ] {
        let (fx, _t) = promote_fixture(&format!("promote-{name}"), 2, "");
        let mock = MockTransport::new(vec![response.into(), "second call must never happen".into()]);
        let lane = FakeLane::new(vec![LaneAction::Commit("lib.rs", RENAMED)]);
        assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done, "{name}");
        assert_eq!(mock.requests_served(), 1, "{name}: retry_cap is 2, but one substantive failure hands over");
        assert_eq!(lane.launches(), 1, "{name}");
        assert!(lane.prompt(0).contains(needle), "{name}: the lane gets the typed failure: {}", lane.prompt(0));

        let state = fs::read_to_string(fx.root.join("run/step-00-rename.json")).unwrap();
        assert!(state.contains("\"promoted\": true"), "{name}: {state}");
        let trail = fs::read_to_string(fx.root.join("run/ledger.jsonl")).unwrap();
        assert!(trail.contains("\"promote\""), "{name}: {trail}");
    }
}

#[test]
fn a_promotion_after_a_commit_tells_the_lane_the_work_stands() {
    let (fx, target) = promote_fixture("promote-sha", 2, "");
    let base = head(&target);
    let mock = MockTransport::new(vec![WRONG_EDIT.into()]);
    let lane = FakeLane::new(vec![LaneAction::Commit("lib.rs", RENAMED)]);
    assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done);

    // The one-shot's commit sha, verbatim, plus D19's fix-forward framing —
    // without it a lane reverts or redoes real work.
    let one_shot_sha = head_before_lane(&target, &base);
    let prompt = lane.prompt(0);
    assert!(prompt.contains(&one_shot_sha), "the committed sha must be named: {prompt}");
    assert!(prompt.contains("That work STANDS"), "{prompt}");
    assert!(prompt.contains("Fix forward"), "{prompt}");
}

/// The first commit after `base` — the one-shot attempt's own.
fn head_before_lane(target: &std::path::Path, base: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["-C", target.to_str().unwrap(), "rev-list", "--reverse", &format!("{base}..HEAD")])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).lines().next().unwrap().to_string()
}

#[test]
fn parse_and_transport_failures_retry_one_shot_and_never_promote() {
    let (fx, _t) = promote_fixture("promote-parse", 2, "");
    // Two parse failures then a good ChangeSet: no lane, all three one-shot.
    let good = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}]}"#;
    let mock = MockTransport::new(vec!["not json".into(), "still not json".into(), good.into()]);
    let lane = FakeLane::new(vec![]);
    assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 3, "parse errors are cheap and self-correcting");
    assert_eq!(lane.launches(), 0, "a parse error is not evidence the model misread the file");
}

#[test]
fn transport_failures_have_their_own_counter() {
    // retry_cap = 0 is ONE model attempt. An exhausted mock is a transport
    // error every time, and those must not consume it — run 1 lost two of
    // three attempts to proxy timeouts at zero tokens.
    let (fx, _t) = promote_fixture("promote-transport", 0, "");
    let mock = MockTransport::new(vec![]);
    let lane = FakeLane::new(vec![]);
    assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done);
    assert_eq!(fx.transcript_lines(), 3, "the transport counter, not retry_cap, bounds this");
    assert_eq!(lane.launches(), 0, "a dead proxy is not substantive");
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("3 transport failures"), "{parked}");
}

#[test]
fn the_park_record_keeps_the_ranked_best_failure_not_the_last_one() {
    // No on_fail here: this is the plain retry ladder. A red gate over
    // committed work outranks the find-miss that follows it, so run 2's most
    // valuable report is not overwritten by a weaker later failure.
    let fx = Fixture::new("rank", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 2)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();
    let mock = MockTransport::new(vec![WRONG_EDIT.into(), FIND_MISS.into(), FIND_MISS.into()]);
    assert_eq!(run(&fx.cfg(), &mock).unwrap(), Outcome::Done);
    assert_eq!(mock.requests_served(), 3);

    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("gate is red"), "the red gate must survive: {parked}");
    assert!(parked.contains("\"gate_report\""), "{parked}");
}

#[test]
fn a_write_to_a_new_path_still_succeeds_one_shot() {
    let fx = Fixture::new("newfile", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    let program = execute_program(&target, 0).replace(
        r#"gate = ["grep -q new_name lib.rs", "! grep -q old_name lib.rs"]"#,
        r#"gate = ["test -f added.rs"]"#,
    );
    fs::write(fx.root.join("programs/program.toml"), program).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "add a file next to:\n{{file:lib.rs}}").unwrap();
    let new_file = r#"{"writes": [{"path": "added.rs", "content": "fn added() {}\n"}]}"#;
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![new_file.into()])).unwrap(), Outcome::Done);
    assert!(!fx.root.join("run/parked.jsonl").exists(), "new-file writes are still one-shot work");
    assert!(target.join("added.rs").is_file());
}

#[test]
fn both_tiers_failing_parks_with_a_reason_naming_each() {
    let (fx, _t) = promote_fixture("promote-park", 2, "");
    let mock = MockTransport::new(vec![WRONG_EDIT.into()]);
    let lane = FakeLane::new(vec![LaneAction::Fail("worker hit its budget cap")]);
    assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done);

    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("one-shot execute failed (GateRed)"), "{parked}");
    assert!(parked.contains("the promoted lane then failed"), "{parked}");
    assert!(parked.contains("budget cap"), "{parked}");
}

#[test]
fn the_usd_budget_blocks_a_lane_launch_and_that_park_never_promotes() {
    // A tokens-only guard shows a healthy green bar while lanes spend dollars.
    let (fx, _t) = promote_fixture("promote-broke", 2, "usd_budget = 0.0");
    let mock = MockTransport::new(vec![WRONG_EDIT.into()]);
    let lane = FakeLane::new(vec![LaneAction::Commit("lib.rs", RENAMED)]);
    assert_eq!(run_with(&fx.cfg(), &mock, &lane).unwrap(), Outcome::Done);
    assert_eq!(lane.launches(), 0, "no dollars, no launch");
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("lane budget exhausted"), "{parked}");
}

#[test]
fn span_input_extracts_the_named_lines_including_inside_a_raw_string() {
    let fx = Fixture::new(
        "span",
        "name = \"sp\"\n[[step]]\nname = \"read\"\nopcode = \"generate\"\nmodel = \"mock\"\ntemplate = \"t.md\"\ninputs = [\"span:kernel.rs:3-4\"]\n",
        &[("t.md", "explain:\n{{span:kernel.rs:3-4}}")],
    );
    // The target line lives inside a raw string, where `anchor:`'s Rust-item
    // matching cannot reach — the exact case that forced a 178K-char godfile
    // into every call of the P3 step that failed.
    fs::write(
        fx.root.join("kernel.rs"),
        "const SHADER: &str = r#\"\nkernel void go() {\n  const uint RT_GI_MAX_BOUNCES = 1u;\n  do_work();\n}\n\"#;\n",
    )
    .unwrap();
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec!["ok".into()])).unwrap(), Outcome::Done);
    let transcript = fs::read_to_string(fx.root.join("run/transcript.jsonl")).unwrap();
    assert!(transcript.contains("RT_GI_MAX_BOUNCES"), "{transcript}");
    assert!(transcript.contains("kernel.rs:3-4"), "provenance line: {transcript}");
    assert!(!transcript.contains("const SHADER"), "only the named lines: {transcript}");

    // A range past the end of the file is a loud check finding, not a guess.
    assert!(workflow_runtime::locate::span(&fx.root, "kernel.rs:1-999").unwrap_err().contains("6 lines"));
}

#[test]
fn transform_failure_reason_carries_stdout() {
    // Probe scripts print their RULING on stdout and exit non-zero. Dropping
    // it made the most valuable park of the P3 shakedown unreadable.
    let program = TRANSFORM.replace("tr 'a-z' 'A-Z'", "echo RULING delta=0.4 threshold=0.1; exit 1");
    let fx = Fixture::new("transform-stdout", &program, &[("draft.md", "go"), ("pass.md", "{{draft}}")]);
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec!["hello".into()])).unwrap(), Outcome::Done);
    let parked = fs::read_to_string(fx.root.join("run/parked.jsonl")).unwrap();
    assert!(parked.contains("RULING delta=0.4"), "the ruling must be in the park reason: {parked}");
}

#[test]
fn abandon_blocks_a_rerun_and_reopen_clears_it() {
    let fx = Fixture::new("abandon", ONE_STEP, &[("draft.md", "write a haiku")]);
    let run_dir = fx.root.join("run");
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec!["haiku text".into()])).unwrap(), Outcome::Done);

    workflow_runtime::runner::abandon(&run_dir, "Peter took this over by hand").unwrap();
    assert_eq!(workflow_runtime::status::read(&run_dir).unwrap().state, "abandoned");

    let err = run(&fx.cfg(), &MockTransport::new(vec![])).unwrap_err();
    assert!(err.contains("took this over by hand") && err.contains("--reopen"), "{err}");

    workflow_runtime::runner::reopen(&run_dir).unwrap();
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![])).unwrap(), Outcome::Done);
    let trail = fs::read_to_string(run_dir.join("ledger.jsonl")).unwrap();
    assert!(trail.contains("\"abandon\"") && trail.contains("\"reopen\""), "{trail}");
}

#[test]
fn unpark_requires_a_note_and_the_note_reaches_the_prompt_after_the_gate_report() {
    let fx = Fixture::new("unpark-note", "name = \"placeholder\"\n[[step]]\nname=\"x\"\nopcode=\"gate\"\ngate=[\"true\"]", &[]);
    let target = init_target_repo(&fx);
    fs::write(fx.root.join("programs/program.toml"), execute_program(&target, 0)).unwrap();
    fs::write(fx.root.join("programs/brief.md"), "rename old_name to new_name in:\n{{file:lib.rs}}").unwrap();
    let run_dir = fx.root.join("run");

    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![FIND_MISS.into()])).unwrap(), Outcome::Done);
    let err = workflow_runtime::runner::unpark(&run_dir, "rename", "   ").unwrap_err();
    assert!(err.contains("--note"), "an unpark without reasoning is not a decision: {err}");

    let note = "the find text was invented; quote lib.rs verbatim this time";
    workflow_runtime::runner::unpark(&run_dir, "rename", note).unwrap();
    assert!(fs::read_to_string(run_dir.join("ledger.jsonl")).unwrap().contains(note));

    let right = r#"{"edits": [{"path": "lib.rs", "find": "fn old_name()", "replace": "fn new_name()"}]}"#;
    assert_eq!(run(&fx.cfg(), &MockTransport::new(vec![right.into()])).unwrap(), Outcome::Done);
    let transcript = fs::read_to_string(run_dir.join("transcript.jsonl")).unwrap();
    let last = transcript.lines().last().unwrap();
    // The seed text reaches a model on no path under D19, so the ruling is its
    // own field — and it lands AFTER the fresh gate report.
    let gate_at = last.find("The gate is currently red").expect("fresh report");
    let note_at = last.find("The human who unparked this step ruled").expect("the ruling");
    assert!(gate_at < note_at, "the ruling comes after the fresh report: {last}");
    assert!(last.contains("quote lib.rs verbatim"), "{last}");
    assert!(!last.contains("fn misquoted"), "the stale seed still goes: {last}");
}

#[test]
fn check_warns_on_untitled_steps_without_failing() {
    let fx = Fixture::new(
        "lint-title",
        "name = \"t\"\n[[step]]\nname = \"a\"\nopcode = \"gate\"\ngate = [\"true\"]\n[[step]]\nname = \"b\"\ntitle = \"Second gate\"\nopcode = \"gate\"\ngate = [\"true\"]",
        &[],
    );
    let warnings = workflow_runtime::check::warnings(&fx.root.join("programs/program.toml"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("\"a\""), "{warnings:?}");
    // Titles are advisory: the same program has zero exit-1 findings.
    assert!(workflow_runtime::check::check(&fx.root.join("programs/program.toml"), &fx.root).is_empty());
}
