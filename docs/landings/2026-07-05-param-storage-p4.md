# PARAM_STORAGE P4 (Ableton + OSC onto the manifest, D9) — landed 2026-07-05

**Branch:** wave/param-storage-p4 (merged `--no-ff` into main; the merge commit is the landing SHA — `git log --merges --first-parent main | head`) · **Level reached:** L2 (repro + guard + dispatch unit coverage of both hardware paths) / target L3 — the real-hardware round-trip (an actual Ableton macro / OSC sender moving a user param) is carried as VD-009.
**Doc status line (quoted verbatim):** `**Status:** IN PROGRESS — **P4 SHIPPED (2026-07-05)**: Ableton + OSC resolve param mappings by manifest id against the LIVE manifest, not the frozen registry's positional tables — user-added and glb-imported params are now mappable/addressable (were silently dropped). Repros written first (red), byte-identical bundled OSC addresses proven by guard. P5 (registry containment + library re-save) remains.`

## What changed

Two live-hardware input paths in `manifold-playback` stopped resolving through the frozen preset registry and now resolve against the instance's live `ParamManifest`.

- **Ableton** (`ableton_bridge.rs`): `WriteTarget.param_index: usize` → `param_id: String`. `rebuild_listeners`' three resolution sites (master effect, layer effect, gen param) dropped `preset_definition_registry::try_get(..).id_to_index` + `param_defs` and now do `fx.params.get(mapping.param_id.as_ref())` — existence gates the mapping, `param.spec.{min,max}` gives the MANIFOLD range. `write_to_project` takes `param_id: &str` and writes `fx.set_base_param(param_id, value)` directly (the P2 `nth(param_index)` translation is gone).
- **OSC** (`osc_param_router.rs`): `OscParamTarget::{MasterEffect,LayerEffect,GenParam}` carry `param_id: String`. Registration iterates `fx.params.iter()` / `gp.params.iter()` (every instance param) instead of `0..def.param_count`, and dispatch writes by id. Two new registry helpers `get_osc_address_by_id` / `get_osc_address_for_layer_by_id` build `/{scope}/{prefix}/{param_id}` from the live id — `osc_prefix` remains a type-level template read (a D2 boundary op, explicitly allowed by the P4 brief).

The bug this closes: a param absent from the registry (a user-added slider, or a glb-imported generator param whose id the registry-frozen `id_to_index` never learned) hit the `let Some(idx) = .. else { continue }` and was silently dropped — unmappable in the Ableton mapping UI, unaddressable over OSC. The write funnels already keyed by id (P2); only the *resolution* was still positional.

## Gate results (verbatim)

- `cargo test -p manifold-playback --lib`: **143 passed; 0 failed; 2 ignored.** Includes the four P4 tests.
- `cargo test -p manifold-core --lib`: **319 passed; 0 failed** (a shared crate — two additive registry fns).
- `cargo clippy -p manifold-playback -p manifold-core --all-targets -- -D warnings`: **clean** (exit 0). `--all-targets` to lint the new `#[cfg(test)]` code.
- **Repros written FIRST, run red before the fix** (design "repros first" requirement): `ableton_rebuild_creates_write_target_for_user_param` (write_targets was `[]`), `osc_registers_address_for_user_added_master_param` (the registered set held the registry's phantom params `/master/{prefix}/amount|segs` but not the instance's real `user_glow`). After the fix both pass, plus `osc_dispatch_writes_user_param_by_id` (dispatch writes by id) and the address-stability guard.
- **Address-stability guard green** — `osc_bundled_addresses_are_byte_identical_to_positional` asserts, for a bundled effect, that `get_osc_address_by_id(ty, id)` equals the old positional `get_osc_address(ty, i)` for every param, and that the router registered exactly those. This *proves* the D9 byte-identity claim (the live-rig OSC address contract), rather than asserting it.
- Negative greps (see Deviations for the honest reading): `id_to_index|param_count|param_defs|param_ids` in `manifold-playback/` → **zero real hits** (all matches are comments or the unrelated `layer_id_to_index` layer-index map).

## Deviations from brief

- **The negative grep `rg -n "param_index" crates/manifold-playback/ → 0` is not literally 0 (3 hits), but there are zero live positional param lookups.** The hits are: one doc comment on `WriteTarget.param_id` describing what it replaced, and two `legacy_param_index: None` test-fixture initializers. `legacy_param_index` is a **manifold-core** serde-compat field on `AbletonParamMapping` (the V1.1 `paramIndex` recovery path) that `project.rs`'s post-load resolver translates to `param_id` and clears at load — it never reaches playback's live resolution, which now keys purely on `mapping.param_id`. Removing it is a serde/migration concern (P1-quarantine / P5 territory), not P4's; the brief's substantive intent ("positional param lookups are what must be gone") is met.
- **OSC coverage for a *wholly-unregistered* glb generator type is bounded by prefix availability, not P4.** P4 makes every param on an instance of a *registered* type addressable (its `osc_prefix` is read from the template). A glb generator whose type carries no `osc_prefix` in the registry still yields no OSC address — correct behaviour (no prefix, no address). The Ableton path — the primary "unmappable slider" complaint — has **no** prefix dependency and works for any manifest param regardless of registry. Full OSC addressing of unregistered glb types is a registry-registration matter for P5.

## Shortcuts confessed

- **`WriteTarget.param_id` and `OscParamTarget::*.param_id` are `String`, not the `ParamId`/`Cow` newtype.** Sidesteps threading the `ParamId` type through two private structs; `set_base_param(&str, ..)` takes the borrow directly, and the id is cloned once at rebuild (a structural, not per-frame, op). The `AbletonMappingTarget` variant keeps its own `param_id: ParamId` (used for instance dispatch identity) — a benign duplication, both sourced from `mapping.param_id` at construction.
- **The positional `get_osc_address` / `get_osc_address_for_layer` stay in the registry** — `manifold-app`'s `ui_bridge/state_sync.rs` still calls them to surface a param's address in the UI. Not dead, not in P4's crate scope; a candidate for the same id-keying in a later manifold-app pass.
- **`osc_prefix` is still read from the registry template at registration.** Explicitly sanctioned by the brief (a type-level boundary read under D2); the negative-grep note reserves it until P5 decides the template library's home.

## Verification debt

- **VD-009 opened** — P4 Ableton/OSC by-id resolution: real-hardware round-trip. Both paths are unit-proven (resolution, dispatch, address byte-stability), but not exercised with a live controller: (a) an Ableton macro mapped to a **user-added / glb-generator** param actually moves it in the running app; (b) an external OSC sender hitting `/master/{prefix}/{user_param_id}` moves the param, and existing bundled addresses still land byte-for-byte. Peter owns the L3 live observation (click-script below).
- Carried forward: VD-007 (P2), VD-008 (P3 running-app smoke) remain open.

## Click-script for Peter (≤3 minutes)

1. On a **glb-imported generator** (or any effect with a user-added slider), open the Ableton mapping UI and map an Ableton macro to that slider — **expect:** it maps (before P4 the slider was unmappable / the mapping silently did nothing). Move the macro — the slider follows.
2. Send OSC to `/master/{prefix}/{param_id}` for a **user-added** master-effect param (or the layer form `/layer/{layerId}/{prefix}/{param_id}`) — **expect:** the param moves.
3. Regression check: send OSC to a **bundled** param's existing address — **expect:** byte-identical behaviour to before (no address drift; the live-rig contract holds).
