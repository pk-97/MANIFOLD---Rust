# Parked items — WS2 seat (lane-owned, D-17)

Append-only. Each entry: what, why parked, exact lines, unpark condition.

## P-F1-frame-body — `tick_and_render` stage decomposition is SEMANTIC, not a pure move

**Parked 2026-07-21 (P-F1, lane/ws2-frame).** D7's `frame/{drain,events,sync,push}.rs` cannot be delivered as pure moves this phase.

**Why:** `tick_and_render` (`app_render.rs` 839–4086) is one method; the four stages are inline segments, not helper methods:
- drain `// 1.` 858–1221 · events `// 2.` 1222–3717 · sync `// 3.` 3718–3751 · push `// 4.` 3752–3785.

They share locals across boundaries — `needs_structural_sync` (declared line 1585, written throughout §2 at 1658/1667/1992/1997/2052/2390/2463/2577/3568/3607, read at §3 line 3571), plus `seg`/`frame_t0` profiler cursors (855–856). Pulling any segment into `frame/<stage>.rs` requires a new function boundary that passes those locals in/out, so the moved lines change (body → params) and `move_identity_check` classifies them as residue, not moves. That is semantic function extraction — forbidden as a pure-move commit by D-7, fenced by INV-G4 (no tick-sequence change), and matches the brief's "genuinely new glue shape → STOP and park; do NOT extend the verifier without a decisions ruling."

**Unpark condition:** a daytime semantic phase with a decisions ruling. Two candidate shapes, both proven behavior-identical by the flow suite (`run_ui_flows.py`) + INV-G4 `MANIFOLD_RENDER_TRACE=1`, not by G1:
1. Make each stage a `&mut self` method on `Application` in `frame/<stage>.rs` and thread `needs_structural_sync`/`seg` explicitly through returns/`&mut` params; `tick_and_render` becomes the orchestrator calling them in the SAME order.
2. Or a `FrameCtx` struct carrying the cross-segment locals.

**Delivered instead this phase (pure moves):** `editor_bridge.rs` (graph-editor bridge cluster + drag structs + tests) and `frame/present.rs` (`present_all_windows` / `represent_cached_offscreen` + present helpers). See `docs/landings/2026-07-21-ui-funnel-p-f1-census.md`.

## P-F1-drag-structs — `Bound`/`UnboundNodeParamDrag` stay in app_render.rs (verifier limit)

**Parked 2026-07-21 (P-F1 editor_bridge slice).** The brief wants the drag structs + `bound_node_param_drag_tests`/`unbound_node_param_drag_tests` in `editor_bridge.rs`; they stay in `app_render.rs` instead.

**Why:** their fields are constructed/read by the staying scrub code inside `tick_and_render` (`self.bound_node_param_drag`, `.current_value`, `.node_id`, …). Moving the struct definitions to a sibling module (`editor_bridge`) requires widening every field to `pub(crate)`. `move_identity_check`'s visibility-pair matcher pairs a removed `    node_id: u32,` with the added `    pub(crate) node_id: u32,` — but `node_id: u32,` also appears ~7× elsewhere in the moved code (fn params, test helpers), so git's `--color-moved=plain` colors ALL plain `node_id: u32,` lines as moves on both sides, consuming the removed struct-field twin. The `+pub(crate) node_id: u32,` lines are then orphaned → residue 2, unavoidable without changing the verifier. The brief forbids extending the verifier without a decisions ruling.

**Unpark condition:** a decisions ruling on either (a) a verifier rule that treats `+pub(crate) <field>: <ty>,` as a visibility pair when the unprefixed form appears anywhere on the removed side (smuggle-risk to weigh), or (b) accept the 2 named residue lines under D-9 (reviewer eyeballs). Then move the two structs, their `impl BoundNodeParamDrag`, and the two drag-struct test modules into `editor_bridge.rs`.
