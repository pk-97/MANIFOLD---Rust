# Landing report — PARAM_RANGE_CONTRACT P1–P2 (2026-07-13)

**Design:** `docs/PARAM_RANGE_CONTRACT_DESIGN.md` (same-day design→approval→execution; Fable orchestrating 2 Sonnet workers on `feat/param-range-contract`).
**Status line now reads:** SHIPPED (quoted in the doc header).

## What shipped
- **P1 `23af50b8`** — `RangeContract`/`RangeReason` (manifold-core, additive serde, one-sided bounds); `PrimitiveSpec::PARAM_CONTRACTS` macro field + `EffectNode::param_contract()` (the `boundary_reason` pattern applied to ranges — NOTE: contracts live on the spec side; core `ParamDef.contract` exists for card-level parity and is always `None` in v1, the doc's D2 anchor understated this split); lint (h) rewritten contract-only and promoted to ERROR (hint disagreement = no finding); meta-test `every_range_contract_names_a_real_boundary` with curated table; hint-never-clamps test; **five text-entry clamp sites removed** (app.rs `handle_text_input_commit` + inspector/param tabs) — typing past a hint now works, per Peter's "never restrictive" rule.
- **P2 `ca399599`** — contracts granted: `node.switch_texture.selector` (Index), `.num_inputs` (Count), `node.multi_blend.num_inputs` (Count), `node.connect_nearest.max_edges` (Count) — each with kernel evidence cited in the curated table. **All four §2 suspects rejected on evidence**: max_distance (squared compare, no divide), draw_lines window (ceil().max(1) proof), edge_stretch width (clamp well-defined at 0), rgb_split ±32 (sampler edge-clamps; no physical ±32). ApricotWeather binding default 26.205 → 9.0 (card is truth). Catalog params gained a `contract` field; CARD_AUTHORING.md gained hints-vs-contracts.

## Gates (orchestrator-verified)
57/57 presets clean via check_presets; all ten former lint-h presets + ApricotWeather validate OK with zero range findings; renderer lib 1174 green, core 366 green; catalog in sync; clippy clean; Liveschool round-trip green (P1).

## For Peter
- Bloom, FluidSim2D and the other eight presets are now valid by policy — no card was shrunk, no feel changed.
- Sliders: typed values beyond a hint now stick (were silently clamped at 5 call sites). Knob visual still pins at the rail — span-zoom UX is Deferred.
- Card lints g (defaults disagreement) and f (mux-vs-blend) remain warnings pending your in-app triage.

## Click-script (≤2 min)
1. `cargo run -p manifold-renderer --bin graph-tool -- validate crates/manifold-renderer/assets/generator-presets/FluidSim2D.json --kind generator` → `OK`, no warnings.
2. In the app: open any effect card, type a value beyond the slider's max into its text field → value sticks (previously snapped back).

## Deviations & debt
- Contracts live on `PrimitiveSpec::PARAM_CONTRACTS`, not per-instance ParamDef (worker-reconciled; endorsed — matches the boundary_reason precedent). Core `ParamDef.contract` is reserved, always None.
- No full 185-primitive Enum/index audit beyond the mux family + spot-checks (D6 remove-by-default covers this; a missed index param surfaces as a validate error the day a card escapes it).
- VD entries: none.
