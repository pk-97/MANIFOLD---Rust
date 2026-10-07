# MANIFOLD — Codex

A visual DAW and live performance instrument. Rust is authoritative; do not consult the archived Unity project.

## Working together

Be concise. Lead with the outcome; explain what a change means for the instrument. No routine action logs, mandatory planning ceremony, or documentation of every edit. Act within the agreed scope. State failures and unverified behaviour plainly.

The lead task owns design, diagnosis, review, and landing regardless of model. Use native Luna subagents (`gpt-5.6-luna`, high effort) for independent mechanical work with a decided fix shape. Each brief names the scope, established findings, reuse target, acceptance criteria, and exact checks. Workers do not delegate or land. Use parallel Luna lanes for independent scopes; handle tiny fixes directly. The lead leaves the lane’s files alone until it returns. Stop repeated failures and return evidence to the lead.

Prepare nontrivial worker briefs with `scripts/codex_prepare.py` and pass its output to the worker. Use `scripts/codex_checks.py` for diff-based worker check selection; unmapped scopes need the lead's judgement. These tools supply context and commands, not permission or a replacement for the landing gate.

CC and `k3m` are separate setups. Do not change `CLAUDE.md`, `.claude/`, Claude settings, shell aliases, or provider configuration unless explicitly requested. Existing shared scripts may be used. Codex guards live in `.codex/hooks.json` and apply equally to all models; no lane registration is required. Read `.codex/README.md` for enforcement limits. Claude hook registration stays separate.

## Engineering essentials

- Content thread owns mutable project/playback state. UI receives snapshots and sends `ContentCommand`s. Project edits go through `EditingService` and undoable commands; no UI model writes.
- No new `Arc<Mutex<>>` or `Arc<RwLock<>>` without approval.
- Beats are primary; timing signatures use `Beats`, `Seconds`, and `Bpm`. `sync_clips_to_time()` is the playback-state authority.
- Preserve write-time non-overlap, undo/redo, serialization compatibility, and MIDI phantom ordering/channel guards.
- No per-frame allocations on hot paths. Reuse scratch buffers, use `AHashMap` for hot ID lookups, and dirty-check UI updates.
- GPU access goes through `manifold-gpu`. Native Metal is the current backend; do not introduce wgpu. Verify backend details against code and focused docs.
- Reuse existing infrastructure. Fix the cause; name any intentional stopgap. No silent fallbacks, unexplained dead-code suppressions, or ignored tests.
- Keep serialized fields camelCase, typed IDs transparent, and runtime fields skipped. Follow local Rust conventions; never blanket-format the repository.

## Read on demand

Use source code to resolve stale documentation; read only the relevant subsystem material.

- Design work: `docs/DESIGN_AUTHORING.md` and `docs/DESIGN_DOC_STANDARD.md`.
- Effects/generators: `docs/DECOMPOSING_GENERATORS.md` in full, then `docs/ADDING_PRIMITIVES.md`. Audit existing primitives before adding one; preserve composability and required fusion proofs.
- GPU/freeze: `docs/MANIFOLD_GPU_ARCHITECTURE.md`, `docs/FREEZE_COMPILER_MAP.md`.
- Transport/sync: `docs/CORE_ENGINE_MAP.md`; frame pacing: `docs/VSYNC_AND_FRAME_PACING.md`.
- Parameter UI: `docs/WIDGET_TREE_DESIGN.md` and `crates/manifold-ui/src/param_surface.rs`; reuse the existing parameter surface.

## Validation and delivery

Keep broad and visual execution usage bounded. Do not repeat passed checks
without changed code or new evidence. Failed checks need an evidence-driven next
step; continue while the next attempt changes code or adds evidence, and report
when no justified next step remains. No optional GPU exploration, broad
test/render sweeps, or extra tasks without explicit scope. Computer use and
rendering need a named behaviour that requires observation: reproduce and verify
as needed to establish the behaviour; stop if the result is inconclusive and
report the gap. Preserve required landing checks. Use the Codex guard's
short-lived, exact-command exceptions only for necessary bounded checks with a
concrete reason, never to bypass their bounds.

Parallel agents share one Mac: 14 cores and one GPU. Prefer small CPU reference tests (8³ or 16³ lattices, hand-built cases) that run in seconds for numerical diagnosis. Codex may launch and control the app, run focused GPU proofs, and inspect renders for bounded reproduction and verification of a named behaviour. Serialize GPU execution through `scripts/gpu_queue.py` or an existing runner that owns the same lock; the lead coordinates app sessions and worker checks on the shared GPU. Compile outside the GPU lock. If sandbox access blocks a required command, request an escalation for that exact command; report any remaining blocker without treating it as a blanket ban or bypassing the lock. Order the work in stages: for each one, edit, `cargo check`, CPU proof, required GPU/visual verification, commit. Run one cargo command at a time, with `CARGO_BUILD_JOBS=4` and a test filter, never a whole-crate test run.

Start diagnosis with the relevant seam. Runtime claims need logs/reproduction; visual claims need an observed render. Use bounded probes when static evidence is insufficient. A green compile does not establish behaviour.

Keep main runnable. App changes use the existing slot ring (`scripts/agent-worktree.py`), with one owner per workstream and a verified base tip. Read `.claude/GIT_TREE_DISCIPLINE.md` for slot, build-lock, and merge mechanics; do not modify it. Preserve unrelated work. Commit exact paths only; no blanket staging, force-push, or destructive history rewrites.

Finish the worktree lifecycle before ending a workstream: release landed slots; retire inactive unfinished slots with `scripts/agent-worktree.py retire SLOT` after reviewing the archive contents and destination. Retirement preserves source and handoff notes on a verified remote archive branch before clearing the checkout and cache. Unknown untracked files require explicit inclusion. An archive is unverified work, never an app landing. Do not leave a handoff note as the only preservation step. Acquire and release scrub inactive caches; never reclaim a live checkout or delete unique ignored assets. This retirement policy supersedes the older rule that unlanded branches must occupy a slot indefinitely.

Run focused clippy and module-scoped tests for changed Rust code. The landing gate maps each changed file to its module filterset, sibling test modules, and named integration binaries; it keeps the GPU lock for these selected tests because device use cannot be known cheaply. When GPU proofs are selected, tests and catalog checks use the same proof feature to reuse the renderer build. Gate and GPU-proof builds set `CARGO_INCREMENTAL=0` for sccache reuse across slots. Comment/blank-only and docs-only diffs skip build, clippy, CPU-test, and GPU-proof legs. Use `scripts/gpu_proofs_gate.py` for GPU-path changes; GPU proofs use cargo test, not nextest. Use `scripts/landing_gate.py` before landing app changes; broad touched-crate and workspace checks belong to `scripts/trunk_health.py`. Documentation/config-only changes need appropriate syntax, reference, and diff checks, not an app build. Avoid repeating passed checks without new evidence.

The landing gate runs every check and prints a `rerun:` command per red (`--fail-fast` stops at the first); fix reds with those focused commands, then run the gate once more to land. Cheap prerequisites run before builds and rendering; the GPU lock is taken once, after every compile, for the flow gate, tests and proofs together. GPU checks run the focused set `scripts/gpu_scope.py` maps from the touched paths plus a fixed smoke set, capped at 360s of test time; an unmapped GPU path fails. glTF import paths also select `glb_conformance`. The full renderer suite runs nightly (`gpu_proofs_gate.py --all`). Check progress is live and failure transcripts are retained.

Commit and push completed, verified work. Workers return edits and check results; the lead reviews and commits. App landings use `scripts/land_branch.py` to run the existing gate before merge/push. Give Peter the exact launch command when testing a worktree build. Track discovered unfinished engineering work in beads; update existing contracts when behaviour changes. Keep history in git and avoid duplicate status prose.
