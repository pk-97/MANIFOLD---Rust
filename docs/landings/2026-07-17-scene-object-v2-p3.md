# SCENE_OBJECT_AND_PANEL_V2 P3 — landed 2026-07-17

**Branch:** wave/scene-object-v2 · **Level reached:** L2 (rosetta import PNG +
duplicate-pair PNG, both read by the orchestrator).
**Doc status line (quoted verbatim):** see the design doc header, updated in
the same commit as this report.

## Gate results (verbatim)

```
cargo nextest run --workspace
     Summary [  14.556s] 3591 tests run: 3591 passed, 12 skipped

cargo clippy --workspace -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 34.95s   (clean)

cargo deny check bans
bans ok

cargo test -p manifold-renderer --features gpu-proofs --lib
test result: ok. 1786 passed; 0 failed; 23 ignored; 0 measured; 0 filtered out; finished in 35.41s

rg -n '"mesh_\{' crates/manifold-renderer/src/node_graph/gltf_import.rs
729:        let mesh_node_id = format!("mesh_{k}");   (an internal node-id string, not a
                                                        render_scene wire port — confirmed
                                                        by reading the surrounding code)

python3 .claude/hooks/bug_status.py   -> only pre-existing, not-mine drift flagged
                                          (BUG-185 filed under Open despite FIXED status,
                                          from the concurrent conformance-fix lane)
```

All independently re-run and verified by the orchestrator, not just the
worker's self-report — including reading the three demo PNGs directly.

## Deviations from brief

1. **BUG-211 ID collision.** The P3 worker logged its `DuplicateSceneObjectCommand`
   string-binding gap as BUG-211. A different concurrent session
   (`lane/bugfix-210-conformance-frozen-time`) independently claimed BUG-211
   for an unrelated conformance-harness fix and landed on main first while
   P3 was still running. Caught during the pre-merge sync (origin/main had
   moved again), renumbered to BUG-212 (the next actually-free ID at merge
   time) before landing — three references (two in the backlog, one in a
   code comment) updated together.
2. **Merge conflict in `docs/BUG_BACKLOG.md`.** Both branches inserted a new
   `### BUG-NNN` entry at the same anchor point (top of `## Fixed`). Git's
   conflict markers additionally exposed a pre-existing formatting bug in
   the OTHER lane's own commit (its BUG-211 entry's last paragraph had
   swallowed the `### BUG-207` heading text that should have preceded the
   next entry — verified by reading origin/main's raw file directly, not
   assumed from the conflict view). Resolved by keeping both new entries
   intact and restoring the missing BUG-207 heading; did not otherwise
   touch the other lane's content.
3. Test runs (`cargo test`/`cargo nextest`) transiently dirtied 12 unrelated
   golden PNGs in `tests/fixtures/gltf/goldens/` both before and after the
   merge — reverted each time before committing (not this wave's content,
   a pre-existing side effect of running the GPU test suite in this
   environment, not investigated further).

## Shortcuts confessed (rolled up from the phase report)

`RemoveSceneObjectCommand`'s known one-hop gap (ungrouped hand-built scene
objects aren't fully cleaned up) was left as-is — extending it needs a
general exclusive-upstream-subgraph reachability search, correctly judged
out of mechanical-spec scope and not attempted ad-hoc. The duplicate-demo
test manually propagates the source's resolved model-file path onto the
clone's mesh nodes as a demo-only workaround for BUG-212 — not present in
the shipped `DuplicateSceneObjectCommand` itself.

## Verification debt

None newly opened by the orchestrator. BUG-212 (duplicate breaks string
bindings on imported objects) is real product debt, logged, not yet fixed —
tracked in `docs/BUG_BACKLOG.md`, not this file (it's a real bug, not an
unclosed verification gap).

## Click-script for Peter (≤2 minutes)

1. Import `tests/fixtures/gltf/the_rosetta_stone.glb` — expect: one
   `node.scene_object` per resulting object, no migration fires (fresh
   imports are already shaped correctly).
2. Click "+ Object" in the scene panel on any project — expect: the new
   object renders (previously it was invisible, BUG-210).
3. Duplicate a hand-built (non-imported) scene object — expect: a second
   copy appears offset from the original, both visible. (Duplicating an
   *imported* glTF object currently loses its geometry — BUG-212, known,
   not yet fixed — don't demo that path.)
4. Rename an object via the panel's rename field — expect: the object's
   handle and its enclosing group (when one exists) both update in one
   undo step.
