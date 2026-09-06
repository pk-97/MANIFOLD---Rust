# MANIFOLD — Codex

A visual DAW and live performance instrument. Rust is authoritative; do not consult the archived Unity project.

## Working together

Be concise. Lead with the outcome; explain what a change means for the instrument. No routine action logs, mandatory planning ceremony, or documentation of every edit. Act within the agreed scope. State failures and unverified behaviour plainly.

The lead task owns design, diagnosis, review, and landing regardless of model. Use native Luna subagents (`gpt-5.6-luna`, low effort) for independent mechanical work with a decided fix shape. Each brief names the scope, established findings, reuse target, acceptance criteria, and exact checks. Workers do not delegate or land. Start with one Luna lane at a time; handle tiny fixes directly. The lead leaves the lane’s files alone until it returns. Stop repeated failures and return evidence to the lead.

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

Keep execution usage bounded. Do not repeat passed checks without changed code
or new evidence. After two failed attempts, stop and report evidence instead of
continuing speculative fixes. No optional GPU exploration, broad test/render
sweeps, or extra tasks without explicit scope. Computer use and rendering need
a named behaviour that requires observation: at most one reproduction and one
verification per fix; stop if inconclusive and report the gap. Preserve required
landing checks. Use the Codex guard's short-lived, exact-command exceptions only
for necessary checks with a concrete reason, never to evade its attempt budget.

Start diagnosis with the relevant seam. Runtime claims need logs/reproduction; visual claims need an observed render. Use bounded probes when static evidence is insufficient. A green compile does not establish behaviour.

Keep main runnable. App changes use the existing slot ring (`scripts/agent-worktree.py`), with one owner per workstream and a verified base tip. Read `.claude/GIT_TREE_DISCIPLINE.md` for slot, build-lock, and merge mechanics; do not modify it. Preserve unrelated work. Commit exact paths only; no blanket staging, force-push, or destructive history rewrites.

Run focused clippy and tests for changed Rust crates. Use `scripts/gpu_proofs_gate.py` for GPU-path changes; GPU proofs use cargo test, not nextest. Use `scripts/landing_gate.py` before landing app changes; broad nightly checks belong to `scripts/trunk_health.py`. Documentation/config-only changes need appropriate syntax, reference, and diff checks, not an app build. Avoid repeating passed checks without new evidence.

Commit and push completed, verified work. Workers return edits and check results; the lead reviews and commits. App landings use `scripts/land_branch.py` to run the existing gate before merge/push. Give Peter the exact launch command when testing a worktree build. Track discovered unfinished engineering work in beads; update existing contracts when behaviour changes. Keep history in git and avoid duplicate status prose.
