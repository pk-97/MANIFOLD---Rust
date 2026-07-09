# PROJECT_FILE_INTEGRITY P1–P3 — landed 2026-07-09 @ merge into main (branch tip `05247ab1` + paper-trail commit)

**Branch:** `feat/project-file-integrity` · **Level reached:** L2 (P2 refuse message + P3 toast strings produced & read) / L1 (P1 durability, fault-injection-bound) — target L2/L4
**Doc status line (quoted verbatim):** `**Status:** SHIPPED — P1–P3 all landed 2026-07-09 (Opus-orchestrated Sonnet/medium wave; closes BUG-062/063/064/065). Durability (P1 fsync) ships at L1 — verified by code inspection + negative gate, not fault injection (VERIFICATION_DEBT). · 2026-07-09 · Opus (1M) · Peter in the room`

Closes **BUG-062** (forward-version guard), **BUG-064** (fsync durability), **BUG-065** (64-bit
hash). **BUG-063** → PARTIAL (silent-repair *visibility* shipped; rescue-journaling deferred).

## What shipped

- **P1** (`050e3fd7` + footgun fix `9668329e`): single-source `CURRENT_PROJECT_VERSION` const in
  `manifold-core`; `save_v2_archive` fsyncs the temp file's contents before the atomic rename;
  `compute_hash` widened to 64 bits. The migrate ladder stays all-literals with the
  ladder-top==const invariant pinned by `test_v100_chains_through_to_v140` (a const bump without a
  matching migration rung now fails the suite — prevents a silently-skipped migration).
- **P2** (`1e349bf5`): forward-version guard at the top of `load_project_from_json_with` (before
  migrate) — a file whose `projectVersion` exceeds this build's is refused with `LoadError::TooNew`,
  surfaced through the existing load-error modal (no app change). Coarse secondary guard on archive
  `format_version`.
- **P3** (`05247ab1`): load-repairs accumulate a `LoadReport` (`#[serde(skip)]` transient field on
  `Project`) and, when non-empty, raise a non-blocking "Opened with repairs: …" toast via a new
  `ProjectIOAction.notice`. No repair's behavior changed.

## Gate results (verbatim, run by the orchestrating session in the worktree)

```
cargo test -p manifold-io -p manifold-core -p manifold-app:
  manifold-app: test result: ok. 163 passed; 0 failed; 2 ignored
  manifold-core: test result: ok. 334 passed; 0 failed
  manifold-io lib: ok. 40 passed  |  forward_version_guard: ok. 7 passed
  history_snapshots: ok. 5 passed |  load_project: ok. 15 passed  |  load_report: ok. 3 passed
cargo build --workspace: Finished (clean — new LoadError::TooNew + ProjectIOAction/Project fields
  broke no downstream match/call site)
cargo clippy --workspace -- -D warnings: Finished; 0 Rust warnings in our crates
  (the 2 AVFoundation lines are pre-existing ObjC-header warnings from manifold-media's build script)
Negative gates:
  rg '"1.11.0"' project.rs migrate.rs → only the const def + intermediate thresholds + test asserts
    (both TARGET literals gone)
  rg 'result[0], result[1], result[2]' manifold-io → 0
  rg 'sync_all' archive.rs → 2 (temp-file + parent-dir)
  rg 'fn is_version_less_than' manifold-io/src → 1 (pub(crate), not duplicated)
  rg 'load_report' project.rs → field carries #[serde(skip)]
```

## Deviations from brief
One, disclosed and corrected: P1 (following doc §3.1 as originally written) wired
`CURRENT_PROJECT_VERSION` into the final migration rung. The orchestrator caught this as a
version-bump footgun (the then-intermediate rung would stamp the newer version → the new rung's
gate skips → a migration silently never runs) and fixed it at `9668329e`; doc §3.1/D3 corrected to
match. P1 also regenerated `docs/README.md` (the design-doc commit had left the docs index out of
sync) — mechanical, via `scripts/gen_docs_index.py`.

## Shortcuts confessed (rolled up from phase reports)
P1: none (beyond the docs-index regen above). P2: none. P3: none — `strip_unknown_effects` and
`repair_overlapping_clips` gained return values (zero behavior change, proven by existing tests
staying green plus the new repair-still-happened assertions).

## Verification debt
VD-021 opened — P1 power-loss durability is L1 (code inspection + negative gate), not
fault-injection-proven; consciously carried. No other VD opened; none closed.

## Scope-honesty note for Peter
BUG-063's original backlog fix shape asked for a *blocking* acknowledge dialog **and** journaling
the pre-repair `project.json` into `history/` as a rescue snapshot. P3 shipped the lighter,
non-blocking toast (design §3.6 decision) and deferred the rescue path (design Deferred §6). So the
*silent* half is closed; a one-restore-away rescue of pre-repair data is not built. Flagging in case
you want the rescue-journaling as a fast follow.

## Click-script for Peter (≤2 minutes)
1. Open your normal current show file — expect: opens normally, **no** version error, no "repairs"
   toast (a clean file reports nothing).
2. Open an older project (pre-1.11 format, e.g. a Burn/Liveschool fixture) — expect: opens and
   migrates forward silently, no error.
3. Open a project known to carry an overlapping clip or a removed/unknown effect — expect: a
   non-blocking toast "Opened with repairs: N …" naming what changed; the project opens.
4. (Forward-refuse — needs a *newer* build than the one opening, so not testable today) The message
   a future old build will show, verbatim: "This project was saved by a newer version of MANIFOLD
   (project format 1.XX) than this build can open (1.11.0). Update MANIFOLD to open it."
