# FUSION_SOTA batch 3 (P6 + P7, wave close) — landed 2026-07-14 @ d37d1cfc

**Branch:** feat/fusion-sota · **Level reached:** L2 (P6, has a census/dispatch-count-observed
surface) / L1 (P7, behavior-identical by construction, observable is the negative gate) — both
at their stated targets (§10)

**Doc status line (quoted verbatim):**
> **Status:** SHIPPED · 2026-07-14 · Fable 5 design (with Peter in the room) · Sonnet 5 executing
> ... All seven phases (P1–P7) now shipped; the design is closed.

## Gate results (verbatim)

**P6** (`3cd54734`) — full gpu-proofs suite: 1559 passed, 8 failed (same 8 pre-existing failures
documented since P5, re-verified unchanged via `git stash` at pre-P6 HEAD). Full `--lib`: 1221
passed, 0 failed. `cargo nextest run --workspace`: 3331 passed. Clippy clean. Census multi-output
family: 3→0 refusals. `fusion_coverage_baseline` raised 33/55/222→33/55/225. Escalation found and
fixed IN-PHASE (not a Peter escalation — a correctness bug the phase's own narrowing would have
introduced, caught by re-running `grouped_presets_fuse_through_entry_points` before/after): cut
rule 6's narrowing let a multi-output node's two ports bridge two otherwise-separate branches via
an excluded gather wire, which `build_region` then wholesale-refused; fixed with a gather-bridge
guard in `partition_regions`'s union step, mirroring the existing cycle-convexity check.

**P7** (`01febe55`) — freeze suite: 77 passed, 0 failed, 3 ignored (includes 2 new LRU tests +
`freeze_has_no_leaks`). Full `--lib`: 1224 passed, 0 failed. Full gpu-proofs `--lib`: 1562 passed,
8 failed (same 8 pre-existing, verified via diff that this phase never touched `codegen.rs`).
`cargo test --test gpu_proofs --features gpu-proofs`: 36 passed. `cargo nextest run --workspace`:
3334 passed. Clippy (`-p manifold-renderer` AND `--workspace`) clean. Negative gate `rg
'Box::leak' crates/manifold-renderer/src/node_graph/freeze/`: **zero hits**, independently
re-verified by the orchestrating session. This is the widest mechanical diff in the wave (12
files) — migration was compiler-driven throughout; the one apparent misfit (`ParamTarget::Node`'s
`&'static str` field) resolved cleanly via `Cow<'static, str>`, an already-established pattern in
the same file (`ParamTarget::Composite::outer_name`), not an adapter/shim.

Full crate + workspace re-verification after P7 (orchestrating session): `cargo test -p
manifold-renderer --lib`: 1224 passed, 0 failed, 4 ignored. Freeze suite: 77 passed, 0 failed, 3
ignored. `cargo check --workspace`: clean. `cargo clippy -p manifold-renderer -- -D warnings`:
clean.

Full workspace sweep at landing (run in the main checkout at merge `d37d1cfc`): `cargo clippy
--workspace -- -D warnings`: clean (only pre-existing manifold-media Objective-C SDK deprecation
warnings). `cargo nextest run --workspace`: 3335 tests run, 3335 passed, 12 skipped. `cargo deny
check bans`: ok. Origin/main had moved twice more during this final batch (BUG-156 backlog entry,
EDITOR_WINDOW_UNIFICATION P1 landing) — merged origin/main into feat/fusion-sota first
(`4dc9679f`), reran the freeze suite + touched-crate clippy (renderer + app, since the merge
touched `app_render.rs`), then landed. This closes the FUSION_SOTA_DESIGN wave: P1–P7 all shipped
across three landings (`ce6dcba8`, `9ceb4aab` fill-in / `78d897cb`, `7fe110ac` fill-in / `d37d1cfc`).

## Deviations from brief

- P6: found a real region-forming regression the narrowing itself would introduce, fixed within
  the same phase (not escalated — this was P6's own bug to fix, not a design-scope question).
- P7: none from the brief. The call-site inventory in the design doc's §1 audit was completely
  stale by execution time (every migration since P1 rewrote install.rs) — re-derived fresh per
  the doc's own prescribed re-derivation command, as expected for a seam-brief phase this deep in
  the build order.
- Orchestrating session (wave close): supersession sweep on `FREEZE_COMPILER_MAP.md`'s §11 honest
  edges — marked #1 (marker ABI), #7 (leak model), #8 (segment worker hang) FIXED, matching the
  doc's existing convention for #3–#5. Retired the `fusion-sota-wave-prompts` memory handoff file
  per its own "delete after both land" instruction (both wave 1's fusion-sweep and this wave have
  now landed).

## Shortcuts confessed (rolled up from phase reports)

- P6: none.
- P7: none.

## Verification debt

None opened across the whole wave (P1–P7). Every phase hit its stated target level with hard
gates green; the one genuine mid-wave gap (P4a's draw_dots not literally forming a region due to
the Color-param finding) was resolved by reordering P5 ahead of P4b, not carried as debt.

## Escalations across the whole wave (summary, for the record)

One real escalation to Peter, mid-wave: P4a found all six `draw_*` atoms (BUG-114's targets) carry
a `Color` param that independently boundary-cuts them from fusing, separate from the `BufferIndex`
mechanism P4a built. FUSION_SOTA's original D4 scoped P5 to Vec3 only. Escalated; Peter approved
expanding P5 to Vec4/Color too and reordering P5 before P4b. Recorded in `docs/FUSION_SOTA_DESIGN.md`
(commit `21794f5c`) and this wave's batch 2 landing report. No other design-level escalations —
P6's region-forming bug was an implementation defect the phase itself introduced and fixed, not a
scope question.

## Click-script for Peter (≤2 minutes)

1. `cat docs/BUG_BACKLOG.md` (search BUG-114) — expect: Status FIXED, in the Fixed section, index
   row shows `~~BUG-114~~ FIXED`.
2. `cat docs/fusion_census.md` — expect: param-type family 19→10, multi-output family 3→0,
   buffer-index-shaped family 22→16 (all lifts this wave delivered).
3. `rg 'Box::leak' crates/manifold-renderer/src/node_graph/freeze/` — expect: zero output (proves
   P7's leak-model closure).
4. `cargo test -p manifold-renderer --lib` — expect: all green (1224 tests).
5. No visual change anywhere in the app — this whole wave is compiler-internals hardening plus
   dispatch-count reduction; the payoff is headroom on heavy show graphs, not appearance.
