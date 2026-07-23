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
- 2026-07-23 rt-t1: pre-existing clippy `redundant_field_names` x2 in crates/manifold-renderer/src/node_graph/preset_runtime/tests/chain_fusion.rs (present on base ea1d8d80, workspace --tests only) — will fail the landing clippy sweep; fix at wave/rt-t1 landing.
