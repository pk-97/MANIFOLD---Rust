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

use std::collections::HashMap;

use manifold_core::effect_graph_def::{BindingDef, BindingTarget, ParamSpecDef};

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