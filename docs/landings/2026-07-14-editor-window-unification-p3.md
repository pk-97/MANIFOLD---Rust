# EDITOR_WINDOW_UNIFICATION P3 — committed on `feat/editor-window-unification` 2026-07-14 (not yet merged to main)

**Branch:** feat/editor-window-unification · **Level reached:** L1 / target L1 (§10 — "this phase's surface is the test itself")
**Doc status line (quoted verbatim, as updated by this phase):** `**Status:** SHIPPED 2026-07-14 — all phases complete on feat/editor-window-unification, pending orchestrator merge to origin/main. P1 (shared tree_passes.rs::render_tree_overlay_passes, D1/D2/D4, BUG-151 FIXED) already LANDED on main @ c8584b8d. P2 (D6 redraw-keepalive aggregate + perf-HUD-in-editor demo) committed @ 9be53956, not yet merged. P3 (D7 structural guard tree_render_call_sites_are_allowlisted + I2 fold-in + this supersession sweep) committed this phase, not yet merged.`

## Read-back (D7, restated)

D7: the structural guard is a workspace meta-test — outside a small compositor allowlist, any `render_tree_range`/`render_sub_region` call site in `manifold-app` fails the suite. Rationale: prose rules don't stop a hurried future window author; a red test does. Precedent for a repo-discipline meta-test in the default sweep: `crates/manifold-core/tests/docs_index_sync.rs` (found via `rg -l "docs_index" crates/`) — it scans the filesystem directly (doesn't link the crate under test) and fails the whole `nextest` suite on drift. `tree_render_call_sites.rs` follows the same shape: a `tests/` integration test that reads `.rs` source as text, no dependency on `manifold-app` having a `lib` target (it doesn't — bin-only crate, confirmed via `Cargo.toml`).

## Re-derived call-site inventory (diverges from D7's baked list — stated explicitly, per brief)

Command: `rg -n "render_tree_range\(|render_sub_region\(" crates/manifold-app/src/` (recursive; the search included `ui_snapshot/mod.rs` inside its subdirectory).

Actual results (2026-07-14, this worktree, after P1+P2):
```
crates/manifold-app/src/editor_frame.rs:309:        ui_renderer.render_tree_range(&ui_root.tree, 0, ui_root.overlay_region_start);
crates/manifold-app/src/tree_passes.rs:11: (doc comment, not a call)
crates/manifold-app/src/tree_passes.rs:132:        ui_renderer.render_sub_region(&ui_root.tree, start, end, false);
crates/manifold-app/src/tree_passes.rs:140:        ui_renderer.render_tree_range(&ui_root.tree, start, usize::MAX);
crates/manifold-app/src/ui_snapshot/mod.rs:1751/1755: (doc comments, not calls)
crates/manifold-app/src/ui_snapshot/mod.rs:1857:                renderer.render_sub_region(&ui.tree, start, end, false);
crates/manifold-app/src/ui_snapshot/mod.rs:1859:                renderer.render_tree_range(&ui.tree, start, end);
```
Definitions: only in `crates/manifold-renderer/src/ui_renderer.rs` (`fn render_tree_range` / `fn render_sub_region`), confirmed by `rg -n "fn render_tree_range|fn render_sub_region" crates/`.

**Divergence from D7's design-time baked inventory:** D7 listed `ui_frame.rs:702,710` as still needing to move into `tree_passes.rs` at P3-impl-time. That move already happened — in P1, not P3 (see the P1 landing report: P1's `tree_passes.rs` was extracted verbatim from `ui_frame.rs:646-716`+`:889-891`). `rg` today finds **zero** raw call sites left in `ui_frame.rs` — the file no longer needs an allowlist entry for this guard at all. I built the allowlist from what actually exists, not from the stale design-time inventory:

| File | Real call count | Allowlisted count | Classification |
|---|---|---|---|
| `tree_passes.rs` | 2 | 2 | the shared pass itself (D1) |
| `editor_frame.rs` | 1 | 1 | editor's narrowed base-content scan `[0, overlay_region_start)` (D2) |
| `ui_snapshot/mod.rs` | 2 | 2 | BUG-097 traversal-semantics proof (named exemption in D7) |
| `ui_frame.rs` | 0 | — (not in allowlist) | fully extracted by P1; no entry needed |
| everything else in `manifold-app/src/` (75 `.rs` files total) | 0 | 0 (default) | — |

`ui_cache_manager.rs`/`ui_renderer.rs` hits are in `manifold-renderer`, a different crate — out of this guard's scope by construction (the test only walks `manifold-app/src/`), matching D7's stated exclusion.

## Entry-state check

`git log --oneline -1` in the worktree: `9be53956` — P2's commit, matching the brief's stated entry state ("P1 landed+pushed to origin/main; P2 committed in this worktree but not yet landed/pushed"). `git status` clean before starting.

## What was built

`crates/manifold-app/tests/tree_render_call_sites.rs` — three tests, no dependency on a `manifold-app` lib target (there isn't one):

1. `tree_render_call_sites_are_allowlisted` — D7's guard. Walks every `.rs` file under `manifold-app/src/` recursively, counts occurrences of `.render_tree_range(` / `.render_sub_region(` (the leading `.` requirement excludes doc-comment prose mentions like `` `render_tree_range(start, end)` `` — no leading dot — while still catching every real method call), and compares against a `BTreeMap<&str, usize>` allowlist keyed by **file path relative to `src/`**, never line numbers. A file not in the map defaults to an expected count of 0. Any mismatch (new file with a raw call, or an allowlisted file's count changing) fails with a message naming the file, the found count, the expected count, and the whole allowlist.
2. `tree_passes_branches_on_input_presence_not_caller_identity` — I2 folded in, per the brief. Scans `tree_passes.rs` for `WorkspaceKind`/`is_graph_editor`/`is_primary`, but only on non-comment lines (a line is a comment if its trimmed text starts with `//`) — the module doc for `TreeOverlayInputs` legitimately *names* this forbidden pattern in prose (documenting the exact rule the test enforces), and a literal zero-hits-anywhere check would have failed against P1's own landed, correct code. Confirmed this is the right call: the P1 landing report's own manual rg check hit the same doc comment and called it "not an instance of it."
3. `guard_walk_finds_source_files` — a sanity check on the walk itself (>50 `.rs` files found, `tree_passes.rs` present) so a broken directory walk can't make the other two tests vacuously pass.

## Gate commands + output (including the mandatory red-then-green probe)

**Baseline, landed tree — GREEN:**
```
$ cargo nextest run --workspace -E 'binary(tree_render_call_sites)'
Starting 3 tests across 1 binary (55 binaries skipped)
    PASS [   0.006s] (1/3) manifold-app::tree_render_call_sites tree_passes_branches_on_input_presence_not_caller_identity
    PASS [   0.006s] (2/3) manifold-app::tree_render_call_sites guard_walk_finds_source_files
    PASS [   0.010s] (3/3) manifold-app::tree_render_call_sites tree_render_call_sites_are_allowlisted
Summary [   0.010s] 3 tests run: 3 passed, 0 skipped
```

**Probe inserted** — a second raw call added immediately after the legitimate D2 call in `editor_frame.rs`:
```rust
ui_renderer.render_tree_range(&ui_root.tree, 0, ui_root.overlay_region_start);
// P3 GUARD PROBE — temporary, reverted immediately after proving the
// meta-test fires red. Do not land this line.
ui_renderer.render_tree_range(&ui_root.tree, 0, ui_root.overlay_region_start);
```

**RED, probe in place:**
```
$ cargo nextest run --workspace -E 'binary(tree_render_call_sites)'
Summary [   0.011s] 3 tests run: 2 passed, 1 failed, 0 skipped
    FAIL [   0.011s] (3/3) manifold-app::tree_render_call_sites tree_render_call_sites_are_allowlisted
  stderr:
    thread 'tree_render_call_sites_are_allowlisted' panicked at crates/manifold-app/tests/tree_render_call_sites.rs:107:5:
    Structural guard (EDITOR_WINDOW_UNIFICATION_DESIGN.md D7) tripped:
    editor_frame.rs: found 2 raw render_tree_range/render_sub_region call site(s), expected 1
    (allowlist: {"editor_frame.rs": 1, "tree_passes.rs": 2, "ui_snapshot/mod.rs": 2})
```

**Probe reverted** (`git status --short crates/manifold-app/src/editor_frame.rs` → empty, i.e. byte-identical to the pre-probe state) — **GREEN again:**
```
$ cargo nextest run --workspace -E 'binary(tree_render_call_sites)'
Starting 3 tests across 1 binary (55 binaries skipped)
    PASS [   0.006s] (1/3) manifold-app::tree_render_call_sites tree_passes_branches_on_input_presence_not_caller_identity
    PASS [   0.006s] (2/3) manifold-app::tree_render_call_sites guard_walk_finds_source_files
    PASS [   0.010s] (3/3) manifold-app::tree_render_call_sites tree_render_call_sites_are_allowlisted
Summary [   0.010s] 3 tests run: 3 passed, 0 skipped
```

**Clippy** (`cargo clippy -p manifold-app -- -D warnings`): clean.

**Full default sweep** (`cargo nextest run --workspace`): `3338 tests run: 3338 passed (1 leaky), 12 skipped` — 9.885s. Consistent with prior workspace-sweep counts; no regressions.

## Supersession sweep

**This doc's Status line** — updated, quoted verbatim above.

**BUG-151 backlog Status** — already `FIXED` (set by P1, `docs/BUG_BACKLOG.md:1436`). Confirmed correct; not re-touched.

**`popup-professional-pass-prompt` memory's prompt 3** — outside this repo (the memory directory is not part of the git worktree); per the task's explicit instruction, this sub-item is the orchestrator's job, not executed here.

**`rg "BUG-151|editor.*overlay" docs/`** — full results reviewed. Findings:
- `docs/BUG_BACKLOG.md` — correct (FIXED entry, plus the unrelated BUG-152/BUG-154 entries that happen to mention overlay/editor in their own right — not stale).
- `docs/EDITOR_WINDOW_UNIFICATION_DESIGN.md` — this doc itself; Status line fixed by this phase, body already correct from P1.
- **`docs/README.md` — WAS STALE, FIXED.** The generated index's one-line summary for this doc (pulled from the doc's own "Prerequisites" sentence) still read `"BUG-151 is OPEN and…"` — a leftover from before P1 corrected the doc's Prerequisites text, because P1/P2 never re-ran `python3 scripts/gen_docs_index.py` after that edit. Regenerated (`python3 scripts/gen_docs_index.py` — "Wrote docs/README.md — 159 docs indexed"); the line now reads `"BUG-151 was fixed by…"`. This is exactly the class of drift the docs-index freshness test protects against structurally (doc list, not content) — this particular staleness was a content-summary drift the meta-test's design doesn't catch, found by the manual `rg` sweep as instructed.
- All other hits (`REALTIME_3D_DESIGN.md`, `PRESET_LIBRARY_DESIGN.md`, `OVERLAY_SESSIONS_AND_PICKER_DESIGN.md`, `AUDIO_MODULATION_DESIGN.md`, `NODE_GROUPS_CANVAS_BUILD.md`, `UI_HARNESS_UNIFICATION_DESIGN.md`, `UI_WIDGET_UNIFICATION_DESIGN.md`, `HARNESS_FIDELITY_INVARIANT_PROPOSAL.md`, historical `docs/landings/*.md`) — unrelated "overlay" mentions (different mechanisms, or correctly-dated historical records of what happened at the time) or already-accurate references to this design. No other stale claims found.

## Shortcuts taken

None.

## Demo artifact (L1 — this phase's surface is the test itself)

The red-then-green probe transcript above IS the acceptance demo: GREEN on the landed tree, RED with the probe in place (naming the exact file, found-vs-expected count, and the full allowlist), GREEN again after a clean revert.

## Anything needing escalation

None. No genuine escalation hit during this phase.

## Commit

`cargo clippy -p manifold-app -- -D warnings` clean; `cargo nextest run --workspace` — 3338 passed, 12 skipped, no failures — before committing.
