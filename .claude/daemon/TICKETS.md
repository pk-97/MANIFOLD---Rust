# Daemon plumbing tickets — Sonnet-buildable

Compiled by Fable 2026-07-07 (final authoring pass) from
eval/observations.session.jsonl. Each ticket is deterministic plumbing — no
judgment calls left inside; where a judgment call existed, the answer is
written here. Build with tests; remember the explicit-dispatch convention
(a test function not added to the file's main() dispatch list silently never
runs — the stop-wait v1 false-green, 2026-07-05). New/changed detection is
UNVALIDATED until a pass grades it. Tickets are independent unless noted.

## T1 — git-landing detection fires on non-git commands (BUG)
Session f204e253 self-graded two FPs: seq 6 fired on a plain `rg` search for
an API name (no git operation in the command at all). The detection regex in
observer.py/common.py evidently matches `cherry-pick`/branch-delete tokens
anywhere in the command string. Fix: anchor the match to a git invocation in
command position (start of command or after `&&`/`;`/`|`), same
command-position discipline preToolUseBash.py already implements — reuse its
tokenizer if importable. Test: `rg 'git branch -D'` (a search ABOUT the
command) must not fire; `git branch -D foo` must.

## T2 — confessed-stopgap: self-disposing-marker exemption (contract landed, runtime pending)
moves.md contract now exempts an added marker whose surrounding added text
names its own concrete disposal trigger ("delete after <named event>",
"convert to a mechanism assertion with the fix", a named measurement/phase
that retires it). Implement in `detect_stopgap_markers`: when the added text
containing the marker also matches a disposal-trigger pattern within the same
added hunk (suggested regexes: `delete (after|once|when)`, `convert to`,
`until <
non-empty>`, `retire[sd]? (with|after)`), do not fire. Graded-FP
specimens: session 9cd5f0c9 seq 2 + seq 4 (TEMPORARY test scaffolding, both
honored same-session). While here: AUDIT the a5d63eee seq 6 fire — the
observation says a marker matched commit-message-like text naming a
PRE-EXISTING gap ("no headless harness exists for it yet"); "yet" is not in
the marker table, so either the observation misattributes the move or the
scan reaches text it shouldn't. Find which and record in the ticket close.

## T3 — ungrounded-chat-claim: widen artifact vocabulary
Current detection (daemon-stop.py, per moves.md contract) recognizes slash
paths under known roots and ALL-CAPS doc tokens. The ef0c8e89 near-miss
asserted the contents of `moves.md` — a bare relative filename — and would
not have matched. Add: (a) bare filenames with code/doc extensions that exist
in the repo root or `.claude/daemon/` (stat-check before counting, to avoid
firing on generic words like `notes.md` that don't exist); (b) move-id-shaped
tokens (`family/kebab-name`) resolvable against moves.md headings. Keep the
existing never-fire rules (fenced blocks, user-introduced artifacts,
recall-marked text) unchanged.

## T4 — PreToolUse Bash lint: rg -r is a replace, not a line-number flag
Two sessions (84a58ca5, a9e1202b) ran `rg -rn`/`rg -rl` meaning `-n`/`-l`,
and read the silently rewritten match text as real code. In
preToolUseBash.py: when an `rg` invocation carries `-r`/`--replace` bundled
with search-style flags (`-n`, `-l`, no explicit replacement intent), attach
a WARNING (never block): "-r is --replace; output text will be rewritten —
did you mean -n?". Fire on bundled short flags (`-rn`, `-rl`, `-nr`) and on
`-r <pattern>` where the command otherwise looks like a search.

**DONE 2026-07-07 (Sonnet, lane/daemon-tickets-a @ `b2a973d9`).** Added
`rg_replace_lint`, fires on bundled short flags containing `r` (`-rn`,
`-rl`, `-nr`, ...) and on standalone `-r`/`--replace`. 6 new tests, all in
the dispatch list.

## T5 — PreToolUse Bash lint: masked exit status on gate commands
Session 4340cb05: `cargo check | rg ...; echo exit: $?` reported rg's exit;
a background gate ended `| rg ...; echo GATE_DONE` — sentinel echoed
unconditionally, completion looked like success. Lint: a
`cargo`/`pytest`/test-runner command whose output is piped into a filter
(`rg`/`grep`/`head`/`tail`) AND followed by `; echo`/`$?` in the same chain
gets a WARNING suggesting `${PIPESTATUS[0]}` or `&&` sequencing. Never block.

**DONE 2026-07-07 (Sonnet, lane/daemon-tickets-a @ `b2a973d9`).** Added
`masked_exit_status_lint` + `_segments_with_ops` (an operator-preserving
segmenter — plain `_shlex_segments` discards which operator joined two
segments, which this shape needs). Fires when a test/build-runner segment
pipes into a filter head and a later `;`-joined segment echoes a status or
`$?`. 5 new tests, all in the dispatch list.

## T6 — preToolUseBash: landing merges inside compounds
Session 4340cb05: a fetch→merge→merge --no-ff→push compound landed a merge on
another session's branch because HEAD changed between steps and was never
re-verified. Extend the landing-protocol guard: a `git merge --no-ff` in the
MAIN checkout inside a compound chain (other commands joined by `&&`/`;`)
gets the allow-with-reminder upgraded to a WARNING that the merge step must
re-verify `git branch --show-current` immediately before it — or better,
deny compounds where a landing merge follows a branch-mutating step with no
`branch --show-current` between them. Deny only that narrow shape; plain
landing chains that start from a verified-main state stay allow+reminder.

**DONE 2026-07-07 (Sonnet, lane/daemon-tickets-a @ `b2a973d9`).** Picked the
"or better" option — `detect_unverified_compound_landing_merge` DENIES (not
just warns) a compound where a landing merge follows an earlier
branch-mutating segment (checkout/switch/merge, reusing
`_is_branch_switch_sub`) with no `git branch --show-current` /
`rev-parse --abbrev-ref HEAD` re-verification segment in between; wired
into `main()` right before the pre-approved-allow branch. Single landing
merges and verified compounds are unaffected. 4 new tests, all in the
dispatch list.

## T7 — MEMORY.md compaction: single-owner protocol
Session 2b15501e hit 4 consecutive file-modified-since-read Edit failures
while a concurrent session compacted MEMORY.md, and a stale full-file Write
was only stopped by the Write tool's own guard. Wherever the compaction
behavior is configured (hook or skill), add: skip compaction when another
live session's daemon pidfile exists (`verdicts/*.pid` — same liveness test
preToolUseBash's shared-checkout guard uses), and when compacting anyway,
mandate re-read + targeted Edits, never a full-file Write from an earlier
read.

**SKIPPED 2026-07-07 (Sonnet, T1-T11 plumbing session) — discovery found no
build site.** `rg -il compact .claude` (excluding verdicts/) turns up only
`GIT_TREE_DISCIPLINE.md`'s unrelated worktree-handoff mention; `rg -l
"MEMORY.md" .claude` returns nothing. There is no hook or skill anywhere
under `.claude/` that runs, schedules, or configures MEMORY.md compaction —
it's the harness's built-in auto-memory behavior, driven by system-prompt
instructions (the "auto memory" section + this repo's CLAUDE.md guidance),
not a code path in this repo. Per the ticket's own instruction ("if it isn't
where the ticket assumes, write findings ... and stop that lane"), there is
nothing to add a pidfile-liveness check to and no compaction call site to
gate — the incident's actual backstop (the Write tool's own
modified-since-read guard) already did its job per the ticket's own
description. Not built; would need a harness-level change outside this
repo's control, or a CLAUDE.md-prose mitigation instead of a code fix.

## T8 — PreToolUse Bash lint: trailing # marker swallows chained commands
Session c9e4d45d: a self-grade append chained after a `#grep-ok` marker
silently never ran (# comments to end of line). Lint: a `#word` token
followed by more command text (`&&`, `;`, `|`, or another command) on the
same line gets a WARNING naming the swallowed text. Never block.

**DONE 2026-07-07 (Sonnet, lane/daemon-tickets-a @ `b2a973d9`).** Added
`trailing_comment_swallow_lint`, reuses `sanitize()` so a `#` inside a
quoted string never counts; fires only when the swallowed text after `#`
contains a shell operator (`&&`/`;`/`|`/`||`), not on a bare trailing
comment. 4 new tests, all in the dispatch list.

## T9 — implement mechanical/stale-brief (advice tier)
Contract in moves.md (2026-07-07). Observer-side: on a live (non-catchup)
Read of a path matching `*_QUEUE.md`, `*BRIEF*.md`, `PASS*_AGENDA.md`,
`docs/handoff*`, or memory `handoff_*.md`, stat the file's mtime; if older
than 48h, fire `mechanical/stale-brief` through the advice frame
(`<daemon-advice>` wrapper, no ack, never escalates), once per (session,
path) — key paths like unread-edit's path sets. mtime is stat'd at event
time by the observer process (hooks may stat; workflow scripts may not — this
is observer code, allowed). Firestate: not needed (catchup rebuilds path
sets; advice fires are cheap to re-arm on revive, matching unread-edit's
convention).

## T10 — ledger annotation: hook warnings attached to tool results
Wakes the dormant anchor/unheeded-warning (moves.md 2026-07-07). In
common.py's ledger rendering: when a tool result carries a PreToolUse hook
warning (the shared-checkout guard's WARNING text, the landing-protocol
reminder), append a `(hook-warning: <first ~80 chars>)` annotation to that
event's ledger line — same mechanism as the stopgap `(adds: ...)` annotation.
Bump WINDOW_VERSION (content-addressed cache keys change for affected windows
only — by design, precedent: the Agent-model annotation). The classifier can
then see the warning; the move's signature does the rest. Specimen the move
must catch: 5363065f — `git checkout -b` in the main checkout with the
warning attached, no weighing sentence, Peter intervened manually.

## T11 — test suites leak records into live telemetry (third purge this week)
Subprocess-style tests (test_worker_nudges' real-subprocess delivery test,
test_stop_valve's hook-invocation tests) execute the real hooks, which write
the REAL telemetry.jsonl and verdicts/ — module-attribute monkeypatching
cannot reach a child process. Purge history: 30 sess-* records (sleep pass
1), 1 test-session record + 9 test-ses/sess1 records (pass 2 night-half,
the latter from the pass's own suite runs). Fix: valve.py (and anything a
hook reads paths from) honors env overrides — DAEMON_TELEMETRY_PATH,
DAEMON_VERDICTS_DIR — and every subprocess-invoking test sets them to temp
paths; in-process tests keep the existing module patching. Test: run the
full suite, assert live telemetry.jsonl byte-identical before/after.
