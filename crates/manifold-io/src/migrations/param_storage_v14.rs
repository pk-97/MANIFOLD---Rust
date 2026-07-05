//! V1.4 param-wire migration — `paramValues` (+ parallel `baseParamValues`)
//! collapse into one id-keyed `params` map. See
//! `docs/PARAM_STORAGE_DESIGN.md` §4 (D4/D5) for the full contract this
//! module implements; this doc comment covers the mechanics only.
//!
//! Operates on the `serde_json::Value` project tree, BEFORE typed
//! deserialization, for both the V1 JSON and V2 ZIP containers — both
//! converge on `crate::migrate::migrate_if_needed` before either format's
//! typed `Project` deserialize ever runs, so wiring in there covers both.
//!
//! This module is the ONLY place positional param knowledge survives.
//! `crate::effects::PresetInstance`'s typed (de)serialize understands only
//! the V1.4 `params` shape; the four legacy shapes below are deleted from
//! there, not moved — this module owns their memory forever.
//!
//! ## The four legacy `paramValues` shapes (D4)
//!
//! Historically `paramValues` was untagged-enum polymorphic over container
//! shape (positional `Array` vs id-keyed `Object`) crossed with per-element
//! shape (bare `f32` vs `{value, exposed}` object):
//!
//! - V1.0/1.1 positional bare-float array
//! - V1.2 keyed bare-float map
//! - V1.3 positional `{value, exposed}` array
//! - V1.3 keyed `{value, exposed}` map
//!
//! `baseParamValues` rode the same container-shape duality but always as
//! plain floats (exposure isn't meaningful pre-modulation).
//!
//! ## Baked tables — frozen historical snapshot, never regenerated
//!
//! `LEGACY_PARAM_ORDER` and `LEGACY_PARAM_ALIASES` are literal data,
//! generated ONCE (2026-07-05) from the live `preset_definition_registry`
//! by a throwaway tool
//! (`crates/manifold-renderer/src/bin/gen_legacy_param_order.rs`, deleted
//! after use — see git history for `cb65d698`'s child commit) and pasted in
//! as source. **Never re-run that generation against a later registry
//! state** — the whole point is that this migration stays correct for OLD
//! files even after templates evolve (D4). `WIREFRAME_DEPTH_LEGACY_14` is
//! hand-authored (see its own doc comment) because the shape it describes
//! no longer exists in ANY live registry snapshot — it predates even the
//! generation date above.

use serde_json::Value;

use manifold_core::effect_registration::{ParamAlias, resolve_param_alias};

// ─── Baked LEGACY_PARAM_ORDER (46 effect+generator type ids) ───
//
// Per type id: today's (2026-07-05) static param order, i.e. the id each
// positional slot names — the same order `PresetDef.param_ids` returns for
// that type today. Used as the id source for a positional `paramValues`
// array UNLESS the instance is a generator with its own per-instance
// `graph.presetMetadata.params` (self-contained order, see
// `positional_ids`) or matches the `WIREFRAME_DEPTH_LEGACY_14` special
// case below.
#[rustfmt::skip]
pub(crate) const LEGACY_PARAM_ORDER: &[(&str, &[&str])] = &[
    ("AutoGain", &["amount", "ratio", "sensitivity", "target"]),
    ("BasicShapes", &["line", "scale", "fill", "clip_trigger"]),
    ("BlackHole", &["speed", "cam_dist", "tilt", "rotate", "steps", "disk_inner", "disk_outer", "disk_glow", "scale", "stars", "spin", "particles", "turbulence", "cam_velocity", "freefall"]),
    ("BlobTracking", &["amount", "threshold", "sensitivity", "smoothing", "connect"]),
    ("Bloom", &["amount"]),
    ("ChromaticAberration", &["amount", "offset", "mode", "angle", "falloff"]),
    ("ColorCompass", &["intensity", "reactivity"]),
    ("ColorGrade", &["amount", "gain", "saturation", "hue", "contrast", "colorize", "tint_hue", "tint_saturation", "tint_focus"]),
    ("ConcentricTunnel", &["shape", "line", "rate", "ring_spacing", "clip_trigger", "trigger_mode"]),
    ("DepthOfField", &["amount", "mode", "focus", "focus_x", "width", "blur", "angle", "quality"]),
    ("DigitalDrift", &["amount", "speed", "bands", "rgb_shift"]),
    ("DigitalPlants", &["noise_scale", "anim_speed", "morph", "base_radius", "height", "taper", "torus_radius", "petal_amp", "rot_speed", "box_scale", "cam_dist", "cam_orbit", "cam_tilt", "cam_fov"]),
    ("Dither", &["amount", "pattern"]),
    ("Duocylinder", &["rotate_xy_speed", "rotate_zw_speed", "rotate_xw_speed", "line", "dist", "show_verts", "vert_size", "animate", "speed", "window", "scale"]),
    ("EdgeDetect", &["amount", "threshold", "mode"]),
    ("EdgeStretch", &["amount", "width", "direction"]),
    ("FluidSim2D", &["flow", "feather", "curl", "turbulence", "speed", "contrast", "scale", "count_m", "clip_trigger", "clip_trigger_mode", "anti_clump", "force", "fill"]),
    ("FluidSim3D", &["flow", "feather", "curl", "turbulence", "speed", "contrast", "scale", "count_m", "clip_trigger", "clip_trigger_mode", "size", "anti_clump", "force", "container", "ctr_scale", "cam_dist", "rotate_x", "rotate_y", "rotate_z", "flatten"]),
    ("Glitch", &["amount", "block_size", "rgb_shift", "scanline", "speed"]),
    ("HighlightBoost", &["amount", "gain", "threshold", "knee"]),
    ("Infrared", &["amount", "palette", "contrast"]),
    ("Invert", &["amount"]),
    ("Kaleidoscope", &["amount", "segments"]),
    ("Lissajous", &["freq_x_rate", "freq_y_rate", "phase_rate", "line", "show_verts", "vert_size", "animate", "speed", "window", "scale", "clip_trigger"]),
    ("MetallicGlass", &["feedback", "noise_scale", "noise_speed", "edge_str", "mirror", "displace", "roughness", "light_int", "cam_dist", "cam_orbit", "cam_tilt", "cam_fov", "look_y"]),
    ("Mirror", &["amount", "mode"]),
    ("MriVolume", &["folder", "position", "center", "width", "scale", "invert", "sharpen", "clip_trigger"]),
    ("NestedCubes", &["speed", "filter", "scale", "scatter", "clip_trigger", "mode"]),
    ("NodeGraphTest", &["amount"]),
    ("None", &[]),
    ("OilyFluid", &["speed", "feedback", "noise", "vel_damp", "curl", "relief", "chroma", "contrast", "hue", "sat", "bright", "vel_disp", "col_disp", "mode"]),
    ("ParticleText", &["flow", "feather", "curl", "turbulence", "speed", "contrast", "scale", "count_m", "clip_trigger", "clip_trigger_mode", "anti_clump", "force", "fill", "text_size", "text_strength"]),
    ("Plasma", &["pattern", "complexity", "contrast", "speed", "scale", "clip_trigger"]),
    ("QuadMirror", &["amount"]),
    ("SoftFocus", &["radius", "amount"]),
    ("StarField", &["scale", "density", "size", "brightness", "drift_speed", "drift_x", "drift_y", "twinkle"]),
    ("StrangeAttractor", &["type", "contrast", "chaos", "speed", "scale", "clip_trigger", "count_m", "diffusion", "tilt", "size", "invert"]),
    ("Strobe", &["amount", "rate", "mode"]),
    ("StylizedFeedback", &["amount", "zoom", "rotate"]),
    ("Tesseract", &["rotate_xy_speed", "rotate_zw_speed", "rotate_xw_speed", "line", "dist", "show_verts", "vert_size", "animate", "speed", "window", "scale", "dimension"]),
    ("Text", &["size", "position_x", "position_y", "scale", "h_align", "v_align", "letter_spacing", "line_spacing", "stroke_width"]),
    ("Transform", &["x", "y", "zoom", "rotation"]),
    ("VoronoiPrism", &["amount", "cells", "source_width"]),
    ("Watercolor", &["amount", "displace", "blur", "decay"]),
    ("Wireframe", &["rotate_x_speed", "rotate_y_speed", "rotate_z_speed", "line", "shape", "show_verts", "vert_size", "scale", "clip_trigger"]),
    ("WireframeDepth", &["amount", "density", "width", "z_scale", "smooth", "subject", "blend", "edge_follow"]),
];

// ─── Baked LEGACY_PARAM_ALIASES (21 type ids carrying a non-empty table) ───
//
// Same frozen-snapshot provenance as `LEGACY_PARAM_ORDER` above.
#[rustfmt::skip]
pub(crate) const LEGACY_PARAM_ALIASES: &[(&str, &[ParamAlias])] = &[
    ("AutoGain", &[("punch", Some("sensitivity")), ("response", Some("sensitivity"))]),
    ("BlobTracking", &[("thresh", Some("threshold")), ("sens", Some("sensitivity")), ("smooth", Some("smoothing"))]),
    ("ColorGrade", &[("sat", Some("saturation")), ("tint_sat", Some("tint_saturation")), ("focus", Some("tint_focus"))]),
    ("ConcentricTunnel", &[("scale", Some("ring_spacing")), ("clip_trigger_mode", Some("trigger_mode"))]),
    ("Dither", &[("algo", Some("pattern"))]),
    ("Duocylinder", &[("xy", Some("rotate_xy_speed")), ("zw", Some("rotate_zw_speed")), ("xw", Some("rotate_xw_speed")), ("verts", Some("show_verts")), ("v_size", Some("vert_size")), ("anim", Some("animate"))]),
    ("EdgeDetect", &[("thresh", Some("threshold"))]),
    ("EdgeStretch", &[("dir", Some("direction"))]),
    ("Glitch", &[("block", Some("block_size"))]),
    ("HighlightBoost", &[("thresh", Some("threshold"))]),
    ("Kaleidoscope", &[("segs", Some("segments"))]),
    ("Lissajous", &[("snap", Some("clip_trigger"))]),
    ("MriVolume", &[("slice_axis", Some("folder")), ("slice_pos", Some("position"))]),
    ("NestedCubes", &[("clip_trigger_mode", Some("mode"))]),
    ("Plasma", &[("snap", Some("clip_trigger"))]),
    ("StarField", &[("depth", Some("scale"))]),
    ("StrangeAttractor", &[("snap", Some("clip_trigger"))]),
    ("Tesseract", &[("xy", Some("rotate_xy_speed")), ("zw", Some("rotate_zw_speed")), ("xw", Some("rotate_xw_speed")), ("verts", Some("show_verts")), ("v_size", Some("vert_size")), ("anim", Some("animate"))]),
    ("Transform", &[("rot", Some("rotation"))]),
    ("Wireframe", &[("xy", Some("rotate_x_speed")), ("zw", Some("rotate_y_speed")), ("xw", Some("rotate_z_speed")), ("verts", Some("show_verts")), ("v_size", Some("vert_size"))]),
    ("WireframeDepth", &[("wire_res", Some("amount")), ("wire_resolution", Some("amount")), ("mesh_rate", Some("amount")), ("flow", Some("amount")), ("lock", Some("amount"))]),
];

/// WireframeDepth's oldest (14-slot) positional shape, from before the
/// hardcoded reorder that used to live in `effects.rs`'s
/// `align_to_definition` (`self.effect_type == WIREFRAME_DEPTH &&
/// param_values.len() == 14`, deleted by this same phase — see
/// PARAM_STORAGE_DESIGN.md D4). That reorder only ever fired for exactly
/// 14 elements, so `positional_ids` below only consults this table for a
/// 14-length WireframeDepth array; any other length falls through to the
/// generic `LEGACY_PARAM_ORDER` entry, exactly matching the old code's
/// behavior (no special-case for any length other than 14).
///
/// Old layout: Amount(0) Density(1) Width(2) ZScale(3) Smooth(4)
/// Persist(5) Depth(6) Subject(7) Blend(8) WireRes(9) MeshRate(10)
/// CVFlow(11) Lock(12) Face(13). Persist/Depth/Face had no transferable
/// successor (the old reorder code dropped them / hardcoded a fresh
/// default for the new EdgeFollow slot) — encoded here as `""`, which
/// `positional_ids`/`apply_values` treat as "drop this slot's value".
/// WireRes/MeshRate/CVFlow/Lock map to their contemporary (now-retired)
/// ids; WireframeDepth's own `LEGACY_PARAM_ALIASES` entry above collapses
/// those onto `amount` at LOAD time (this migration does not need to know
/// that — positional resolution and keyed alias resolution are separate
/// steps, per §4).
const WIREFRAME_DEPTH_LEGACY_14: &[&str] = &[
    "amount", "density", "width", "z_scale", "smooth", "", "", "subject", "blend", "wire_res",
    "mesh_rate", "flow", "lock", "",
];

fn static_order(type_id: &str) -> &'static [&'static str] {
    LEGACY_PARAM_ORDER
        .iter()
        .find(|(t, _)| *t == type_id)
        .map(|(_, ids)| *ids)
        .unwrap_or(&[])
}

fn aliases_for(type_id: &str) -> &'static [ParamAlias] {
    LEGACY_PARAM_ALIASES
        .iter()
        .find(|(t, _)| *t == type_id)
        .map(|(_, a)| *a)
        .unwrap_or(&[])
}

/// Migrate every preset instance's `paramValues`/`baseParamValues` in the
/// project tree to the V1.4 `params` shape. Entry point wired into
/// `crate::migrate::migrate_if_needed`.
pub(crate) fn migrate(root: &mut Value) {
    crate::migrate::for_each_preset_instance(root, migrate_one_instance);
}

/// Migrate a single preset instance (effect or generator) JSON object.
/// A no-op if the instance carries neither legacy field (already V1.4, or
/// a bare stub with no param state at all) — makes the whole migration
/// idempotent by construction, matching every other step in this chain.
pub(crate) fn migrate_one_instance(fx: &mut Value) {
    let Value::Object(map) = fx else { return };
    if !map.contains_key("paramValues") && !map.contains_key("baseParamValues") {
        return;
    }

    let is_generator = map.contains_key("generatorType");
    let type_id = map
        .get(if is_generator { "generatorType" } else { "effectType" })
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let param_values = map.remove("paramValues").unwrap_or(Value::Null);
    let base_param_values = map.remove("baseParamValues");

    let mut params = serde_json::Map::new();
    apply_values(map, &type_id, is_generator, param_values, &mut params, false);
    if let Some(bpv) = base_param_values {
        apply_values(map, &type_id, is_generator, bpv, &mut params, true);
    }

    map.insert("params".to_string(), Value::Object(params));
}

/// Resolve the ordered id list a positional array should zip against.
/// See the module doc + `WIREFRAME_DEPTH_LEGACY_14`'s doc comment for the
/// three cases, in priority order.
fn positional_ids(
    instance: &serde_json::Map<String, Value>,
    type_id: &str,
    is_generator: bool,
    array_len: usize,
) -> Vec<String> {
    // Case 1 (§4 step 2, first case): a generator with its own per-instance
    // graph carries the full [bundled | user-added] order in
    // `graph.presetMetadata.params` — self-contained, no table needed. This
    // is what makes the "generator-with-user-bindings positional" shape
    // (the one P1 explicitly tests) resolve correctly.
    if is_generator
        && let Some(meta_params) = instance
            .get("graph")
            .and_then(|g| g.get("presetMetadata"))
            .and_then(|m| m.get("params"))
            .and_then(|p| p.as_array())
        && !meta_params.is_empty()
    {
        return meta_params
            .iter()
            .map(|p| {
                p.get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            })
            .collect();
    }

    // Case 2: WireframeDepth's retired 14-slot shape.
    if type_id == "WireframeDepth" && array_len == 14 {
        return WIREFRAME_DEPTH_LEGACY_14
            .iter()
            .map(|s| s.to_string())
            .collect();
    }

    // Case 3: the baked static order (§4 step 2, "otherwise"), with an
    // effect's per-instance user-added tail appended. Generators never get
    // a tail here — a generator positional array beyond the registry count
    // without its own graph shouldn't occur (the only way a generator grows
    // a tail is via a per-instance graph, handled by case 1), but if it
    // somehow did, the extra values fall through to the "array longer than
    // the table's order" truncate-and-warn policy in `apply_values`.
    let mut ids: Vec<String> = static_order(type_id).iter().map(|s| s.to_string()).collect();
    if ids.is_empty() {
        eprintln!(
            "[param_storage_v14] WARNING: '{type_id}' is not in the baked LEGACY_PARAM_ORDER \
             table — dropping its positional paramValues/baseParamValues entirely (today's \
             unregistered-type policy, matching effects.rs's historical \
             into_positional warning)."
        );
    }
    if !is_generator {
        let tail: Vec<String> = instance
            .get("graph")
            .and_then(|g| g.get("presetMetadata"))
            .and_then(|m| m.get("bindings"))
            .and_then(|b| b.as_array())
            .map(|bindings| {
                bindings
                    .iter()
                    .filter(|b| {
                        b.get("userAdded")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                    })
                    .map(|b| {
                        b.get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default();
        ids.extend(tail);
    }
    ids
}

/// Apply one legacy value container (`paramValues` or `baseParamValues`) —
/// positional array or id-keyed map, per §4 step 1 — into the growing
/// V1.4 `params` map. `is_base` selects `baseParamValues`'s fold-into-`base`
/// semantics (§4 step 4) over `paramValues`'s fresh-entry semantics.
fn apply_values(
    instance: &serde_json::Map<String, Value>,
    type_id: &str,
    is_generator: bool,
    values: Value,
    out: &mut serde_json::Map<String, Value>,
    is_base: bool,
) {
    let field_name = if is_base { "baseParamValues" } else { "paramValues" };
    match values {
        Value::Array(arr) => {
            let ids = positional_ids(instance, type_id, is_generator, arr.len());
            for (i, v) in arr.into_iter().enumerate() {
                let Some(id) = ids.get(i) else {
                    // Array longer than the table's order: truncate, warn
                    // (§4 failure story) — the same posture
                    // `align_to_definition` took today.
                    eprintln!(
                        "[param_storage_v14] WARNING: '{type_id}' {field_name}[{i}] has no \
                         known id — truncating (array longer than the known order)."
                    );
                    continue;
                };
                if id.is_empty() {
                    continue; // explicit drop marker (non-transferable legacy slot)
                }
                merge_entry(out, id, &v, is_base, type_id, field_name, i);
            }
        }
        Value::Object(obj) => {
            // Keyed forms never need the positional table — ids are already
            // ids; only alias resolution applies (§4 step 3).
            for (k, v) in obj {
                match resolve_param_alias(aliases_for(type_id), &k) {
                    Some(resolved) => {
                        let resolved = resolved.to_string();
                        merge_entry(out, &resolved, &v, is_base, type_id, field_name, 0);
                    }
                    None => {
                        eprintln!(
                            "[param_storage_v14] '{type_id}' {field_name} key '{k}' aliases to \
                             a dropped param — value discarded."
                        );
                    }
                }
            }
        }
        Value::Null => {} // absent — nothing to fold.
        other => {
            eprintln!(
                "[param_storage_v14] WARNING: '{type_id}' {field_name} has an unrecognized \
                 shape ({other:?}, neither array nor object) — dropped entirely."
            );
        }
    }
}

/// Merge one resolved `(id, raw value)` pair into the growing V1.4 map.
/// `paramValues` entries create/overwrite `{value, exposed}`;
/// `baseParamValues` entries fold `base` onto whatever entry already
/// exists (creating a default-shaped one first if `paramValues` didn't
/// carry this id — defensive; shouldn't occur in practice since both
/// arrays share the same positional layout / keyed id set).
fn merge_entry(
    out: &mut serde_json::Map<String, Value>,
    id: &str,
    v: &Value,
    is_base: bool,
    type_id: &str,
    field_name: &str,
    index: usize,
) {
    if is_base {
        let Some(f) = v.as_f64() else {
            eprintln!(
                "[param_storage_v14] malformed '{type_id}' {field_name}[{index}] entry for \
                 '{id}' (not a number) — dropped."
            );
            return;
        };
        let entry = out.entry(id.to_string()).or_insert_with(|| {
            Value::Object(serde_json::Map::from_iter([
                ("value".to_string(), Value::from(0.0)),
                ("exposed".to_string(), Value::from(true)),
            ]))
        });
        if let Value::Object(m) = entry {
            m.insert("base".to_string(), Value::from(f));
        }
        return;
    }

    let (value, exposed) = match v {
        Value::Object(m) => (
            m.get("value").and_then(|x| x.as_f64()).unwrap_or(0.0),
            m.get("exposed").and_then(|x| x.as_bool()).unwrap_or(true),
        ),
        other => match other.as_f64() {
            Some(f) => (f, true),
            None => {
                eprintln!(
                    "[param_storage_v14] malformed '{type_id}' {field_name}[{index}] entry for \
                     '{id}' (not a number or {{value,exposed}} object) — dropped."
                );
                return;
            }
        },
    };
    out.insert(
        id.to_string(),
        serde_json::json!({ "value": value, "exposed": exposed }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn migrate_fx(mut fx: Value) -> Value {
        migrate_one_instance(&mut fx);
        fx
    }

    // ── The four legacy `paramValues` shapes (D4) ──

    #[test]
    fn v10_v11_positional_bare_float_array() {
        // "Bloom" -> ["amount"] in LEGACY_PARAM_ORDER.
        let fx = migrate_fx(json!({
            "effectType": "Bloom",
            "paramValues": [0.75]
        }));
        assert_eq!(fx.get("paramValues"), None, "paramValues must be deleted");
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.75));
        assert_eq!(fx["params"]["amount"]["exposed"].as_bool(), Some(true));
    }

    #[test]
    fn v12_keyed_bare_float_map() {
        let fx = migrate_fx(json!({
            "effectType": "ColorGrade",
            "paramValues": { "amount": 0.5, "saturation": 1.2 }
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.5));
        assert_eq!(fx["params"]["saturation"]["value"].as_f64(), Some(1.2));
        assert_eq!(fx["params"]["saturation"]["exposed"].as_bool(), Some(true));
    }

    #[test]
    fn v13_positional_slot_array() {
        let fx = migrate_fx(json!({
            "effectType": "AutoGain",
            "paramValues": [
                { "value": 0.1, "exposed": true },
                { "value": 0.2, "exposed": false },
            ]
        }));
        // AutoGain -> ["amount", "ratio", "sensitivity", "target"]
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.1));
        assert_eq!(fx["params"]["amount"]["exposed"].as_bool(), Some(true));
        assert_eq!(fx["params"]["ratio"]["value"].as_f64(), Some(0.2));
        assert_eq!(fx["params"]["ratio"]["exposed"].as_bool(), Some(false));
        assert!(fx["params"].get("sensitivity").is_none(), "no 3rd slot supplied");
    }

    #[test]
    fn v13_keyed_slot_map() {
        let fx = migrate_fx(json!({
            "effectType": "AutoGain",
            "paramValues": {
                "amount": { "value": 0.3, "exposed": false },
                "target": { "value": 0.9 }
            }
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.3));
        assert_eq!(fx["params"]["amount"]["exposed"].as_bool(), Some(false));
        assert_eq!(fx["params"]["target"]["value"].as_f64(), Some(0.9));
        // exposed defaults to true when absent from the object shape.
        assert_eq!(fx["params"]["target"]["exposed"].as_bool(), Some(true));
    }

    // ── baseParamValues fold-in (D5) ──

    #[test]
    fn base_param_values_folds_into_base_field() {
        let fx = migrate_fx(json!({
            "effectType": "Bloom",
            "paramValues": [0.75],
            "baseParamValues": [0.5]
        }));
        assert!(fx.get("baseParamValues").is_none(), "baseParamValues must be deleted");
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.75));
        assert_eq!(fx["params"]["amount"]["base"].as_f64(), Some(0.5));
    }

    #[test]
    fn keyed_alias_resolves_before_emitting() {
        // WireframeDepth's own alias table: wire_res -> amount.
        let fx = migrate_fx(json!({
            "effectType": "WireframeDepth",
            "paramValues": { "wire_res": 42.0, "density": 3.0 }
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(42.0));
        assert_eq!(fx["params"]["density"]["value"].as_f64(), Some(3.0));
        assert!(fx["params"].get("wire_res").is_none(), "old id must not survive");
    }

    // ── Generator-with-user-bindings positional case ──
    //
    // A generator whose per-instance graph carries a user-added binding
    // beyond the bundled registry count must resolve the FULL positional
    // array (bundled + user tail) through its OWN graph.presetMetadata.params
    // order, not the baked table.

    #[test]
    fn generator_with_user_bindings_positional_case() {
        let fx = migrate_fx(json!({
            "generatorType": "Plasma",
            "paramValues": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 0.42],
            "graph": {
                "presetMetadata": {
                    "params": [
                        { "id": "pattern" },
                        { "id": "complexity" },
                        { "id": "contrast" },
                        { "id": "speed" },
                        { "id": "scale" },
                        { "id": "clip_trigger" },
                        { "id": "user.blur.radius.1" }
                    ]
                }
            }
        }));
        assert_eq!(fx["params"]["pattern"]["value"].as_f64(), Some(1.0));
        assert_eq!(fx["params"]["user.blur.radius.1"]["value"].as_f64(), Some(0.42));
    }

    // ── WireframeDepth 14-slot case ──

    #[test]
    fn wireframe_depth_legacy_14_slot_case() {
        let fx = migrate_fx(json!({
            "effectType": "WireframeDepth",
            "paramValues": [
                1.0, 2.0, 3.0, 4.0, 5.0, // Amount..Smooth
                999.0, 998.0,            // Persist, Depth -- dropped
                7.0, 8.0, 9.0, 10.0, 11.0, 12.0, // Subject..Lock
                997.0                     // Face -- dropped
            ]
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(1.0));
        assert_eq!(fx["params"]["density"]["value"].as_f64(), Some(2.0));
        assert_eq!(fx["params"]["width"]["value"].as_f64(), Some(3.0));
        assert_eq!(fx["params"]["z_scale"]["value"].as_f64(), Some(4.0));
        assert_eq!(fx["params"]["smooth"]["value"].as_f64(), Some(5.0));
        assert_eq!(fx["params"]["subject"]["value"].as_f64(), Some(7.0));
        assert_eq!(fx["params"]["blend"]["value"].as_f64(), Some(8.0));
        // WireRes/MeshRate/Flow/Lock carry over under their retired ids —
        // WireframeDepth's own alias table (loaded later, at reconcile
        // time, not by this migration) collapses them onto "amount".
        assert_eq!(fx["params"]["wire_res"]["value"].as_f64(), Some(9.0));
        assert_eq!(fx["params"]["mesh_rate"]["value"].as_f64(), Some(10.0));
        assert_eq!(fx["params"]["flow"]["value"].as_f64(), Some(11.0));
        assert_eq!(fx["params"]["lock"]["value"].as_f64(), Some(12.0));
        // Persist/Depth/Face (positions 5, 6, 13) have no id — dropped;
        // the other 11 of 14 legacy slots transfer.
        assert_eq!(fx["params"].as_object().unwrap().len(), 11, "only 11 of 14 legacy slots transfer");
        assert!(fx["params"].get("edge_follow").is_none(), "no historical source; loader backfills the template default");
    }

    #[test]
    fn wireframe_depth_non_14_length_falls_through_to_current_table() {
        // A hypothetical 8-length positional array (today's current order)
        // must NOT be run through the 14-slot special case.
        let fx = migrate_fx(json!({
            "effectType": "WireframeDepth",
            "paramValues": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(1.0));
        assert_eq!(fx["params"]["edge_follow"]["value"].as_f64(), Some(8.0));
    }

    // ── Effect positional array with a per-instance user-added tail ──

    #[test]
    fn effect_positional_array_with_user_added_tail() {
        let fx = migrate_fx(json!({
            "effectType": "Bloom", // -> ["amount"]
            "paramValues": [0.5, 0.25],
            "graph": {
                "presetMetadata": {
                    "bindings": [
                        { "id": "user.blur.radius.1", "userAdded": true, "label": "Radius",
                          "defaultValue": 0.0, "target": { "kind": "node", "nodeId": "n", "param": "p" } }
                    ]
                }
            }
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.5));
        assert_eq!(fx["params"]["user.blur.radius.1"]["value"].as_f64(), Some(0.25));
    }

    // ── Failure story ──

    #[test]
    fn unregistered_type_drops_positional_values_without_panicking() {
        let fx = migrate_fx(json!({
            "effectType": "TotallyMadeUpEffectType",
            "paramValues": [1.0, 2.0, 3.0]
        }));
        assert_eq!(
            fx["params"].as_object().unwrap().len(),
            0,
            "unregistered type: values dropped, not defaulted or panicked"
        );
    }

    #[test]
    fn array_longer_than_table_order_truncates_the_extra() {
        let fx = migrate_fx(json!({
            "effectType": "Invert", // -> ["amount"], length 1
            "paramValues": [0.9, 123.0, 456.0]
        }));
        assert_eq!(fx["params"]["amount"]["value"].as_f64(), Some(0.9));
        assert_eq!(fx["params"].as_object().unwrap().len(), 1, "extra positions truncated, not misassigned");
    }

    #[test]
    fn malformed_entry_is_dropped_not_fatal() {
        let fx = migrate_fx(json!({
            "effectType": "ColorGrade",
            "paramValues": { "amount": "not-a-number", "saturation": 1.0 }
        }));
        assert!(fx["params"].get("amount").is_none());
        assert_eq!(fx["params"]["saturation"]["value"].as_f64(), Some(1.0));
    }

    #[test]
    fn already_migrated_instance_is_untouched() {
        let original = json!({
            "effectType": "Bloom",
            "params": { "amount": { "value": 0.75, "exposed": true } }
        });
        let fx = migrate_fx(original.clone());
        assert_eq!(fx, original, "idempotent: no paramValues/baseParamValues means no-op");
    }

    // ── Whole-project walk (integration-style) ──

    #[test]
    fn migrate_walks_master_effects_and_generator_layers() {
        let mut root = json!({
            "projectVersion": "1.10.0",
            "settings": {
                "masterEffects": [
                    { "effectType": "Bloom", "paramValues": [0.6] }
                ]
            },
            "timeline": {
                "layers": [
                    { "genParams": { "generatorType": "Plasma", "paramValues": [1.0, 2.0, 3.0, 4.0, 5.0, 6.0] } }
                ]
            }
        });
        migrate(&mut root);
        assert_eq!(
            root["settings"]["masterEffects"][0]["params"]["amount"]["value"].as_f64(),
            Some(0.6)
        );
        assert_eq!(
            root["timeline"]["layers"][0]["genParams"]["params"]["pattern"]["value"].as_f64(),
            Some(1.0)
        );
    }
}
