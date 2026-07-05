# PARAM_STORAGE P5 — dead positional field shed (registry containment) — landed 2026-07-05

**Branch:** wave/param-storage-p5b (merged `--no-ff` into main) · **Level reached:** L2 (full workspace test + clippy sweep — the infrastructure gate). Second P5 increment, after the P5-partial macro-label slice.

## What changed

`PresetDef` sheds three positional fields and a method that had no live callers left once P2/P4/P5-a moved all resolution onto the `ParamManifest`:

- **`preset_def.rs`** — removed `param_count`, `id_to_index`, `param_ids`, and `index_for_param()`. **Kept `param_defs` and `legacy_param_aliases`** — they serve the two boundary roles (instantiation/template seeding via `get_defaults`; the one-time migration resolver in `project.rs`), which is exactly the "escalate rather than shed quietly" call the design asks for.
- **`preset_definition_registry.rs`** — removed `param_id_to_index` (0 prod callers), `param_index_to_id` (one caller migrated), and the **positional** `get_osc_address` / `get_osc_address_for_layer` (the id-keyed `_by_id` variants from P4 are the replacements). `format_value` now guards on `param_defs.len()`. Both `PresetDef` constructors and the effect/generator registration builders drop the removed field initializers.
- **`app.rs`** text-input handler — resolves the clamp range *and* the param id from a single `fx.params.iter().nth(param_idx)` (resp. `gp.params`) off the live manifest. Fixes two latent bugs in the old split: (a) it could read `old_val` from the manifest param but send the command for a *different* registry id when manifest/registry order diverged; (b) a user param beyond the registry got `param_index_to_id == None`, so its text-edit was silently dropped — now it works, and the clamp uses the calibrated range.
- **`state_sync.rs`** — the card-row OSC addresses use `get_osc_address_by_id(_for_layer)` keyed on the row id. **`ui_snapshot/fixtures.rs`** — read `param_defs[i].id`. **`osc_param_router.rs`** — the P4 byte-identity test asserts `get_osc_address_by_id` against an expected `/master/{prefix}/{id}` string (its positional oracle is gone). Test files (`generator_param_counts.rs`, `command_roundtrips.rs`, `legacy_param_id_resolution.rs`) redirected to `param_defs.len()` / `param_defs[i].id`.

## Gate (verbatim, run independently of the implementing agent)

- `cargo test --workspace --no-fail-fast`: only failure is `manifold-ui design_tokens::no_new_raw_color_literals` (**BUG-030**, 201/200, pre-existing, fails identically on main; my change touches no manifold-ui color literals). Every other crate's tests pass.
- `cargo clippy --workspace --all-targets -- -D warnings`: clean (the manifold-media `cc` ObjC-deprecation warnings are pre-existing and don't gate).
- Negative grep `\.param_count|\.id_to_index|\.param_ids|param_id_to_index|param_index_to_id|index_for_param` across `crates/`: zero live-code hits — the 7 remaining are stale doc-comments (and `layer_id_to_index` / a `param_count:` fn-arg in manifold-ui, both unrelated).

## Process note

The ~20-site refactor was delegated to a Sonnet agent from a precise site map (Peter's "Sonnet for mechanical bulk"); its report was verified independently here — the workspace sweep, clippy, negative grep, and a manual read of the behavior-sensitive `app.rs` diff, per the daemon's `anchor/ungrounded-resolution` nudge (don't trust an agent's self-report).

## Remaining P5 (unchanged from the P5-partial note, minus what shipped here)

- **Inspector single-source migration (deferred, needs a Peter decision).** `state_sync`'s param-card builder (~1900-1980) still reads `param_defs` positionally plus a separate `user_param_bindings` tail, so a **calibrated bundled param shows its uncalibrated range in the card**. Migrating it to the single-source manifest is the correct fix, but the manifest's `param.spec` doesn't carry `is_angle` / `convert`, which today live only on the user bindings — so this needs a data-model decision (put them on the manifest spec, or keep the card builder reading bindings for those two flags). This is a genuine escalation, not a field-shed. **Peter's call.**
- **Library re-save** — the one-time real-project re-save to V1.4, Peter-owned, P5's final proof.

Stale doc-comments referencing the now-deleted helpers (7 sites, listed in the field-shed commit's grep) are harmless narrative; a cleanup pass can fold them in with the inspector work.
