//! Scene-scale derivation for BUG-upfq (Scene-param ergonomics), P1+P2.
//!
//! Imported 3D models come in any real-world scale, but the card sliders
//! stamped from a primitive's generic `ParamDef` used to show hardcoded
//! ranges (-100..100 position, 0..100 focus, 0.01..200 light range) that
//! only fit scenes around unit size. This module derives one `SceneScale`
//! from the import's bbox and the import path applies it to the stamped
//! slider ranges as a display-only override — stored param VALUES are never
//! touched, so existing projects render identically.
//!
//! The scale is the bounding-sphere radius (half the bbox diagonal), the
//! same quantity `build_import_graph` already computes for camera framing.
//! It is floored at 0.01 so a degenerate/empty bbox (zero extent, or a
//! tiny asset at millimeter scale) still gets nonzero slider widths and no
//! divide-by-zero anywhere downstream.
//!
//! SSAO radius is NOT in the range-profile below: `build_import_graph`
//! scene-scales it as a NODE default (`scene.rs` — `0.5·radius`, clamped
//! between `0.001·radius` and `2·radius`) and deliberately does not expose
//! it on the card (Peter 2026-07-15, "the defaults look good"). A future
//! audit should not re-flag it.

use std::collections::HashMap;

use manifold_core::effect_graph_def::{BindingDef, BindingTarget, ParamSpecDef};

use crate::node_graph::primitives::DEFAULT_FAR as CAMERA_FAR_DEFAULT;

/// The single scene-scale fact imported scenes stamp their slider ranges
/// from. Radius in world units; every derived range width is a multiple of
/// it, so a scene twice as big gets sliders twice as wide.
#[derive(Debug, Clone, Copy)]
pub(super) struct SceneScale {
    /// Floor of the bounding-sphere radius (0.01 — the smallest sensible
    /// feature scale for a unitless model). All derived widths multiply
    /// this, so even a degenerate bbox still yields tappable sliders.
    pub radius: f32,
}

/// The smallest per-scene radius we'll ever derive — explicitly the brief's
/// BUG-upfq floor so a zero-extent or all-dimension-collapsed bbox can't
/// divide or hand back a zero-width slider range.
pub(super) const SCENE_SCALE_FLOOR: f32 = 0.01;

impl SceneScale {
    /// Derive from a `(min, max)` bbox — the exact same half-diagonal
    /// computation `build_import_graph` and `merge_import_into_graph` use
    /// for their camera/radius facts, plus the 0.01 floor.
    pub fn from_bbox(min: [f32; 3], max: [f32; 3]) -> Self {
        let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let radius = ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5)
            .max(SCENE_SCALE_FLOOR);
        Self { radius }
    }
}

/// Position slider range (`pos_*` on `node.transform_3d` and `node.light`,
/// `aim_*` on `node.light`): `±2·radius`. One scene-radius covers the whole
/// model from its recentered origin; two gives the model plus its
/// immediate neighbourhood — enough platform to dock something at the edge
/// of the shot, not so much that the useful band squeezes into the slider's
/// first few percent.
pub(super) const POSITION_MULTIPLIER: f32 = 2.0;

/// Focus-distance slider range (`node.camera_lens`): `0..4·radius`. The
/// near end is the hyperfocal pin (focus_distance <= 0 = off); the far end
/// reaches two scene-widths out — the import camera sits at `2.2·radius`,
/// so a full focus pull from the model's face out to the lens is on-slider.
pub(super) const FOCUS_MULTIPLIER: f32 = 4.0;

/// Light attenuation (`node.light` `range`) slider range:
/// `0.01..4·radius`, matching the focus band. `node.light`'s `range` is a
/// soft falloff half-distance (`1/(1+d²/range²)` — 50% at d == range), so
/// capping the slider at 4·radius lets a light fill the whole set while
/// keeping fine control in the near half.
pub(super) const LIGHT_RANGE_MULTIPLIER: f32 = 4.0;

/// F-stop slider range (`node.camera_lens`) top multiplier (BUG-bdwd):
/// `0.5..max(32, 64·radius)`. The radius multiplier is needed because the
/// migration multiplies stored f_stops by the scene radius (look-preserving
/// — blur ∝ world_to_mm/f_stop, so f_stop×R at world_to_mm=1000/R keeps the
/// image identical); a huge scene's migrated values (up to f/64·R) must stay
/// on-slider. 64 is the generic band's top, so unit-scale scenes keep the
/// CinematicScene default band exactly.
pub(super) const F_STOP_MAX_MULTIPLIER: f32 = 64.0;

/// Camera-framing geometry for the synthesized orbit camera, derived from
/// the import bbox. Computed once at the top of `build_import_graph` and
/// shared by every placement decision below it (camera node params, lens
/// focus seed, sun shadow range, per-light reach). This is the "frame the
/// model" half of scene geometry; [`SceneScale`] is the "size the sliders"
/// half.
#[derive(Debug, Clone, Copy)]
pub(super) struct Framing {
    /// Bbox center (scene context `(0,0,0)`).
    pub center: [f32; 3],
    /// Half the bbox diagonal, floored at `1e-3` — the golden-stable
    /// framing floor. Distinct from [`SceneScale::radius`]'s `0.01`
    /// (BLACK floor for slider widths): small-but-real assets keep their
    /// exact pre-fix framing distances and near/far planes.
    pub radius: f32,
    /// Camera vertical FOV — hoisted here so both the framing-distance fit
    /// and the camera node's own `fov_y` param read the SAME value (a
    /// duplicated literal is how BUG-206-style drift happens).
    pub fov_y: f32,
    /// Framing distance: `2.2 * radius` floored against a per-axis
    /// half-FOV fit, so an elongated asset's dominant axis still fits the
    /// frame with margin.
    pub distance: f32,
    /// Orbit-camera near clip: half the front-face gap (`distance - radius`)
    /// with a `1e-4` floor — never in front of the object (BUG-165/BUG-169),
    /// never so deep it costs depth precision at the object (BUG-774a).
    pub near_clip: f32,
    /// Orbit-camera far clip: `distance + 1.5 * radius` floored at the
    /// camera's DEFAULT_FAR (golden-stable for current assets), capped at
    /// `node.orbit_camera`'s declared range max.
    pub far_clip: f32,
}

impl Framing {
    /// Compute all framing facts from a `(min, max)` bbox. Pure math — no
    /// import state. The `1e-3` radius floor and the `1e-4` front-margin
    /// floor keep degenerate/small assets renderable (BUG-165/BUG-169) and
    /// every currently-passing asset's numbers byte-identical to the
    /// historic import.
    pub fn compute(min: [f32; 3], max: [f32; 3]) -> Self {
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let radius =
            ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
        // `2.2 * radius` alone frames by the bbox half-DIAGONAL,
        // which for an elongated (tall/thin) object barely exceeds its dominant
        // axis — the frame's vertical span contains the object with almost no
        // margin, and camera tilt + perspective push it past the top/bottom
        // edges. Frame by PER-AXIS fit instead: for each axis, the distance
        // required so that half the axis's extent subtends no more than the
        // half-FOV, with a 1.15 safety margin. The render aspect isn't known at
        // import time, so the horizontal half-angle is conservatively treated
        // as equal to the vertical one (square-aspect assumption — never
        // UNDER-frames a wider-than-tall render).
        let fov_y = 0.9_f32;
        let half_fov_tan = (fov_y * 0.5).tan();
        let per_axis_fit = dims
            .iter()
            .map(|&extent| (extent * 0.5) / half_fov_tan * 1.15)
            .fold(0.0f32, f32::max);
        // The `2.2 * radius` floor keeps every COMPACT asset's framing
        // IDENTICAL to before this fix (the golden-stability guarantee:
        // per_axis_fit is only ever larger than the floor for objects
        // dominated by one axis, where the diagonal-based distance genuinely
        // under-frames).
        let distance = (2.2 * radius).max(per_axis_fit);
        // BUG-165/BUG-169 root cause (diagnosed via GLB_XFAIL_BURNDOWN_DESIGN.md
        // P1's `--trace` instrument): `node.orbit_camera`'s `near` default was
        // never scaled to the framed object, so for any object with `radius`
        // below ~0.042 (BoomBox, MetalRoughSpheresNoTextures) the fixed near
        // plane sat IN FRONT of the object and the whole frame clipped to black.
        //
        // Fix: `near` always tracks the object's own front-face distance with a
        // 2x safety margin, in BOTH directions. Down keeps tiny assets rendering
        // (BUG-165/169); up keeps 24-bit depth precision at the object's actual
        // distance — BUG-774a (kuma_heavy_robot: front face ~3300 units out,
        // near pinned at the floor gives ~10 units of depth resolution there,
        // so the model's two overlapping mesh shells z-fight into a triangle
        // mosaic). At `front_margin * 0.5` the front face always gets ~2^23
        // depth slices per unit distance, at any scene scale.
        let front_margin = (distance - radius).max(1e-4);
        let near_clip = front_margin * 0.5;
        // `far` is the same class of bug as `near` above, at the opposite end:
        // the orbit camera's fixed default (CAMERA_FAR_DEFAULT, 200) was never
        // scaled to the framed object either, so any asset whose POSED bbox
        // puts geometry past 200 units depth-clips to a black frame at the
        // default framing (kuma_heavy_robot — 100% black until `far` is raised
        // by hand). The DEFAULT floor keeps every currently-passing asset's far
        // IDENTICAL to before; the 10000 ceiling is `node.orbit_camera`'s
        // declared range max, past which the value would clamp at param load
        // anyway.
        let far_clip = CAMERA_FAR_DEFAULT.max(distance + 1.5 * radius).min(10_000.0);

        Self { center, radius, fov_y, distance, near_clip, far_clip }
    }
}

/// Classify and apply the scene-derived range overrides to every stamped
/// card param. `nodes_by_id` maps each binding target's inner `NodeId`
/// string to its primitive type id (the import builds this from its own
/// node inventory). Only plain numeric sliders still bound directly to a
/// generic node param — never the hand-curated fan-out rows — get
/// overridden, and only when their node type + param name are
/// scene-scaleable:
///
/// - `node.transform_3d` / `node.light` `pos_*` / `aim_*` → ±2·radius
/// - `node.light` `range` → 0.01..4·radius
/// - `node.camera_lens` `focus_distance` → 0..4·radius
/// - `node.camera_lens` `f_stop` → 0.5..max(32, 64·radius) (BUG-bdwd — the
///   scene-derived DoF aperture band; see [`F_STOP_MAX_MULTIPLIER`])
///
/// Each override is a range-only edit: `default_value` is never moved (the
/// `.min`/`.max` widen rule only guarantees the range still CONTAINS the
/// stamped default, exactly like the stamper's own widen rule).
pub(super) fn apply_scene_ranges(
    params: &mut [ParamSpecDef],
    bindings: &[BindingDef],
    nodes_by_id: &HashMap<String, String>,
    scale: SceneScale,
) {
    let r = scale.radius;
    // Position is centered on the recentered origin — an object docked at
    // the origin's edge (off in one direction) gets the same band the other
    // way, so the model can always come back to center.
    let position = (-POSITION_MULTIPLIER * r, POSITION_MULTIPLIER * r);
    let focus = (0.0, FOCUS_MULTIPLIER * r);
    // Min pinned at 0.01, the primitive's own range floor — the falloff
    // half-distance never meaningfully goes smaller on a healthy scene.
    let light_range = (0.01, LIGHT_RANGE_MULTIPLIER * r);
    // Scene-relative f-stop band. 0.5 is the lens's photographic floor; the
    // top is 32 (the default band) or 64·radius for a larger scene — a
    // scene-derived f/64+ turns the huge scenes that used to need absurd
    // f-stops into on-slider values (migrated projects carry f_stop × R).
    let f_stop_range = (0.5, (32.0f32).max(F_STOP_MAX_MULTIPLIER * r));

    for spec in params.iter_mut() {
        // Enum/toggle/whole-number sliders are index or label spaces, not
        // physical quantities — never scale them.
        if spec.is_toggle || spec.is_trigger || spec.whole_numbers {
            continue;
        }
        let Some(binding) = bindings.iter().find(|b| b.id == spec.id) else {
            continue;
        };
        let BindingTarget::Node { node_id, param } = &binding.target else {
            continue;
        };
        let Some(type_id) = nodes_by_id.get(node_id.as_str()) else {
            continue;
        };
        let new_range = match type_id.as_str() {
            "node.transform_3d" if matches!(param.as_str(), "pos_x" | "pos_y" | "pos_z") => {
                Some(position)
            }
            "node.light" if matches!(param.as_str(), "pos_x" | "pos_y" | "pos_z" | "aim_x" | "aim_y" | "aim_z") => {
                Some(position)
            }
            "node.light" if param.as_str() == "range" => Some(light_range),
            "node.camera_lens" if param.as_str() == "focus_distance" => Some(focus),
            "node.camera_lens" if param.as_str() == "f_stop" => Some(f_stop_range),
            _ => None,
        };
        let Some((new_min, new_max)) = new_range else {
            continue;
        };
        // Range-only edit; keep the stamped default inside the band.
        spec.min = new_min.min(spec.default_value);
        spec.max = new_max.max(spec.default_value);
    }
}

/// Build the node-id → type-id map the range override needs. Walks every
/// node in `def.nodes` INCLUDING group interiors, the same recursive walk
/// `migrate_scene_exposures` uses — bindings target inner node ids which
/// are unique across the whole def regardless of grouping.
pub(super) fn node_types_by_id(
    nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
) -> HashMap<String, String> {
    fn walk(
        nodes: &[manifold_core::effect_graph_def::EffectGraphNode],
        out: &mut HashMap<String, String>,
    ) {
        for node in nodes {
            out.insert(node.node_id.as_str().to_string(), node.type_id.clone());
            if let Some(group) = node.group.as_deref() {
                walk(&group.nodes, out);
            }
        }
    }
    let mut out = HashMap::new();
    walk(nodes, &mut out);
    out
}