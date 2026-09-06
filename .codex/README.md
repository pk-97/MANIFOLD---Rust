# Codex guards

Trust the project hook with `/hooks` in Codex before using lanes. Hooks are
inactive until trusted; changes to the hook definition require review again.

One native Luna lane at a time, explicit model and low effort. Add one line to
its brief: `MANIFOLD_SCOPE: {"worktree":"/absolute/slot/path","files":["relative/file.rs"]}`.
Use an empty file list for read-only work. Acquire write slots using the existing
ring, from the current main tip. The lead does not edit a lane's files while it
is working. Luna uses native patches, returns results, and never commits or lands.

Native desktop dispatch currently reaches hooks as `collaborationspawn_agent`;
keep that name alongside `spawn_agent` and `Agent` in both matcher and guard.
After changing dispatch handling, verify a live read-only lane as well as tests:
unit tests cannot establish runtime scope registration.

The hook checks both paths of moves, blocks app patches in main, reuses CC's
git/path detection, and requires app landings through `scripts/land_branch.py`.
No approval auto-allows and no per-turn reminders or automatic build loops.

Run `python3 -B .codex/hooks/test_guard.py` after changes. CC files are imported
read-only; their settings, hook registration and state are not modified.

Limits: this is workflow enforcement on supported tool calls, not a security
boundary. Arbitrary lead shell scripts, MCP writes and interactive stdin are
not file-scope checked. Model-based lane identification assumes this two-model
setup; one lane per parent session is essential to its scope tracking. Scope
records are temporary and a missing record blocks lane edits/checks. Changing
the roster or enabling parallel lanes requires revisiting this design.
