# Parked items — WS2 seat (lane-owned, D-17)

Append-only. Each entry: what, why parked, exact lines, unpark condition.

## P-F1-frame-body — `tick_and_render` stage decomposition is SEMANTIC, not a pure move

**Parked 2026-07-21 (P-F1, lane/ws2-frame).** D7's `frame/{drain,events,sync,push}.rs` cannot be delivered as pure moves this phase.

**Why:** `tick_and_render` (`app_render.rs` 839–4086) is one method; the four stages are inline segments, not helper methods:
- drain `// 1.` 858–1221 · events `// 2.` 1222–3717 · sync `// 3.` 3718–3751 · push `// 4.` 3752–3785.

They share locals across boundaries — `needs_structural_sync` (declared line 1585, written throughout §2 at 1658/1667/1992/1997/2052/2390/2463/2577/3568/3607, read at §3 line 3571), plus `seg`/`frame_t0` profiler cursors (855–856). Pulling any segment into `frame/<stage>.rs` requires a new function boundary that passes those locals in/out, so the moved lines change (body → params) and `move_identity_check` classifies them as residue, not moves. That is semantic function extraction — forbidden as a pure-move commit by D-7, fenced by INV-G4 (no tick-sequence change), and matches the brief's "genuinely new glue shape → STOP and park; do NOT extend the verifier without a decisions ruling."

**RULED — D-28 (top session):** park CONFIRMED; becomes daytime semantic phase **P-F1b**. Parity oracles = flow suite (`run_ui_flows.py`) + INV-G4 `MANIFOLD_RENDER_TRACE=1`, not G1. Top-session recommendation: a `FrameCtx` struct carrying the cross-segment locals (mirroring `DispatchCtx`) — but the P-F1b brief decides the shape.

**Delivered instead this phase (pure moves):** `editor_bridge.rs` (graph-editor bridge cluster + drag structs + tests) and `frame/present.rs` (`present_all_windows` / `represent_cached_offscreen` + present helpers). See `docs/landings/2026-07-21-ui-funnel-p-f1-census.md`.

## P-F1-drag-structs — `Bound`/`UnboundNodeParamDrag` stay in app_render.rs (verifier limit)

**Parked 2026-07-21 (P-F1 editor_bridge slice).** The brief wants the drag structs + `bound_node_param_drag_tests`/`unbound_node_param_drag_tests` in `editor_bridge.rs`; they stay in `app_render.rs` instead.

**Why:** their fields are constructed/read by the staying scrub code inside `tick_and_render` (`self.bound_node_param_drag`, `.current_value`, `.node_id`, …). Moving the struct definitions to a sibling module (`editor_bridge`) requires widening every field to `pub(crate)`. `move_identity_check`'s visibility-pair matcher pairs a removed `    node_id: u32,` with the added `    pub(crate) node_id: u32,` — but `node_id: u32,` also appears ~7× elsewhere in the moved code (fn params, test helpers), so git's `--color-moved=plain` colors ALL plain `node_id: u32,` lines as moves on both sides, consuming the removed struct-field twin. The `+pub(crate) node_id: u32,` lines are then orphaned → residue 2, unavoidable without changing the verifier. The brief forbids extending the verifier without a decisions ruling.

**RULED — D-28 (top session): DO NOT MOVE, park CLOSED.** `Bound`/`UnboundNodeParamDrag` are P-I deletion targets (design D4: they die into `ScrubState`); relocating them to `editor_bridge.rs` is churn P-I would delete. They stay in `app_render.rs` until the scrub wire kills them. Neither the verifier extension nor D-9 residue is needed.

## P-RT-lane-naming — RT dispatcher cannot spawn NAMED teammate lanes (harness flat-roster limit)

**Parked 2026-07-22 night (RT wave dispatcher).** `rt-wave-queue.md` line 26 + RT-D1 require each lane to be a NAMED Agent-tool teammate. The Agent tool rejects this from my seat: "Teammates cannot spawn other teammates — the team roster is flat." The RT dispatcher is itself a teammate of the Fable top session, so it sits one level below the team lead and cannot mint named peers.

**Not spine-blocking — proceeding with the functional equivalent:** unnamed Sonnet subagents (subagent_type `claude`, `model: sonnet`, LOW effort, one commit then stop). They run isolated in their slot-ring worktree and return a final report — identical to the named-lane contract except they are not mid-flight addressable via SendMessage. The lane model (one commit, then stop) needs no mid-flight messaging, so the only lost capability is unused. Review/wake of the Fable top session still works normally (SendMessage upward to the team lead is permitted).

**Unpark condition:** Peter/top-session either (a) has the top (Fable) session spawn the named lanes directly, or (b) amends the queue's "named teammate" wording to permit unnamed subagents for a teammate-dispatcher. Until then, unnamed subagents stand.

## T1-C ORBIT gate red — T1-A ghost oracle is sited on a shadow boundary; parallax dominates the metric (design fork, WAVE STOPPED)

**Parked 2026-07-23 (RT Tier-1 dispatcher).** T1-C committed on `wave/rt-t1` @ `f9bc2b30` (slot-0): `accumulate_irradiance` now reprojects history through `prev_view_proj` (world pos from depth + `inv_view_proj`), rejects on depth (5e-3 NDC-z) or normal (cos 0.9, using T1-B's real vertex normal via a new `out_n`/`hi_normal` widen of `trace_shadow_rays`+`upsample_shadow`), history ping-ponged (2 slots each + `rt_history_ping` to avoid an in-dispatch read_write race), disocclusion → current-frame-only. Reset stays on the sole `TemporalResetDetector` (negative-rg verified: only `detect_reset()` OR dimension-change feeds `AccumulateParams.reset` — no second reset path).

**Why parked (design fork, not a mechanical miss):** T1-C's named gate — the T1-A `rt_ghost_orbit_consecutive_frame_luma_diff_exceeds_threshold` oracle flipped live and green — does NOT go green. Pre-fix 0.0444 → post-fix 0.0410, still >> the 0.02 threshold. The lane bisected before committing rather than fake a pass:
- Hardcoding the validity gate to always-accept = a NO-OP on the numbers → reprojection/blending already succeeds every frame; gating is not the bottleneck.
- Zeroing `SOFT_SHADOW_CONE_RADIANS` + 16× AO/GI samples barely moved it (0.041→0.031) → ray noise is not dominant.
- A radius-0 single-pixel probe (vs the oracle's radius-7 window) shows a real STEP crossing partway through the 12-frame sweep, not gradual noise.

Dispatcher-verified corroboration (exit-code, slot-0): the committed suite is 19 passed / 1 FAILED (ORBIT) / 1 ignored (STILL); P2 cut-reset + strobe proofs and T1-B value test all green (T1-C broke nothing). The ORBIT lumas trace is `[0.566, 0.607, 0.632, 0.695, 0.711, 0.715, 0.768, 0.794, 0.852, 0.821, 0.890, 0.955]` — a near-monotonic brightening ramp as the tracked point `[1,0,-1]` (deliberately sited ON the shadow boundary by T1-A to expose same-texel ghosting) transitions occluded→lit under the ~19° orbit. That is the shadow edge sweeping across the probe window (legitimate geometric parallax), which dominates any ghosting residual — it is NOT BUG-311's same-texel defect. Logged BUG-315 in `docs/BUG_BACKLOG.md` with the full trail; BUG-311 left OPEN (lane declined to self-declare "fixed" with the named gate red).

**Design fork for Fable/Peter:** the ORBIT oracle conflates ghosting with real shadow-edge parallax at a boundary point, so it cannot certify BUG-311 either way. Candidate resolutions (Fable's call, this is oracle-design not accumulation work): (a) re-site the tracked point to a spot with a strong irradiance/ghosting gradient but minimal geometric shadow-edge motion under orbit — verify with a radius-0 sweep first; (b) redefine the metric to isolate the temporal-blend contribution, e.g. diff each frame's reprojected-history-blended output against a from-scratch cold render at the same camera (measures ghosting directly, insensitive to legitimate geometry); (c) accept BUG-311 fixed on the bisection evidence + Peter's in-app look and re-`#[ignore]` ORBIT with an updated note. The bisection rules out `accumulate_irradiance` as the cause; no more accumulation work is indicated.

**WAVE STOPPED:** per the dispatcher charter (T1-C park stops the wave — T1-D's entry is a green T1-C on branch, and the branch currently carries a live failing test). T1-D NOT dispatched. `f9bc2b30` left as committed by the lane (honest red state), not amended.

**Unpark condition:** Fable rules the oracle resolution (a/b/c above) and either re-sites/redefines the ORBIT oracle green or re-`#[ignore]`s it with BUG-311 accepted; then T1-D may proceed. BUG-315 is the tracking id.

**UPDATE 2026-07-23 (D19 attempt — metric does NOT discriminate; escalated to Fable again, wave still stopped):** Per Fable's D19 ruling (option b), a test-only lane rebuilt the ORBIT oracle as end-of-orbit ACCUMULATED frame vs COLD-START render at the SAME final camera pose, gradient-region mean abs luma diff. Result: it does NOT separate pre-fix from post-fix. Lane stopped without committing (honored "no threshold tuning"), src restored to f9bc2b30 (verified clean), test rewrite preserved at scratchpad/t1a-revision-scratch.patch.
- Best config (ramp 6 frames @ ORBIT_RATE 0.05 → final_orbit 0.95 mid-boundary, HOLD 1): pre-fix (10359365) 0.0210 vs post-fix (f9bc2b30) 0.0217 — essentially identical, reprojection has no measurable effect on this metric.
- Three other configs same non-discrimination pattern (0.0012/0.0000; 0.0000/0.0000; 0.0179/0.0175).
- Two real gotchas the lane found + fixed in its scratch (worth keeping): (1) if the render ctx's `dt` stays 1/60 while `time` jumps per orbit-frame, `TemporalResetDetector` (node_graph/temporal_reset.rs) sees a discontinuity and RESETS history EVERY frame — silently defeating accumulation in any orbit test; the sweep must pass `dt = per-frame time step`. (2) RT-D4 async accel build (`rt_accel_pending_key`) needs a commit+wait PER FRAME; batching warmup frames into one encoder breaks the state machine (affected only the lane's scratch scan, not the delivered per-frame `render_one`).
- The lane's read (mine too): either (a) T1-C's reprojection genuinely doesn't reduce ghosting for a SLOW CONTINUOUS single-direction pan through a soft shadow gradient (the metric may be honest and BUG-311 not actually fixed by T1-C for this artifact class), or (b) the ramp+hold / cold-reference construction still isn't isolating ghosting (HOLD=1 too short for the fix's benefit to appear; or the cold reference's own warmup settles fix-specific state). Two independent oracles (the confounded consecutive-frame one AND this pose-diff one) now both fail to demonstrate T1-C reduces ghosting — that is the load-bearing new fact.

**Revised unpark for Fable:** this is past oracle-siting — it questions whether T1-C fixes BUG-311 measurably at all via a numeric pose-diff oracle. Options: (i) direct a specific different isolation (e.g. a MOTION pattern T1-C's reprojection was validated against — fast/multi-direction — rather than a slow pan; or measure history-reprojection error directly inside the kernel via a debug readback rather than a whole-frame diff); (ii) accept BUG-311 on Peter's in-app L2 look (no numeric oracle certifies it — re-`#[ignore]` ORBIT with a note pointing at BUG-315 + this finding); (iii) reopen whether T1-C's approach actually addresses the artifact class. T1-D remains blocked. f9bc2b30 unchanged.
