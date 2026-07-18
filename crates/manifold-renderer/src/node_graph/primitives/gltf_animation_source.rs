//! `node.gltf_animation_source` — samples a parsed glTF TRS keyframe
//! clip at a live `progress` (0..1) and emits the nine scalars
//! `node.transform_3d`'s port-shadowed inputs already accept.
//!
//! GLTF_ANIMATION_DESIGN.md A1 (D1): "animating a rigid node is animating
//! params" — this node does no scene-graph work itself; it is a pure
//! CPU sampler that turns keyframe channels into the nine `pos_x/y/z,
//! rot_x/y/z, scale_x/y/z` scalars, wired straight into an object's
//! `node.transform_3d`.
//!
//! GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: keyframe payload no longer lives in
//! this node's Table params (the pre-P2 `translation_track`/`rotation_
//! track`/`scale_track` Tables are DELETED, not just unread — see the
//! phase brief's D5 migration for the pre-P2-preset path). Instead `path`
//! plus THREE per-channel node selectors — `translation_node`/
//! `rotation_node`/`scale_node` — pick each channel out of an
//! `Arc<GltfAnimSet>` from `gltf_anim_cache`'s shared, file-backed,
//! `Weak`-held cache (the SAME cache `node.gltf_skeleton_pose` reads, one
//! entry per FILE, shared across every node/object referencing it). THREE
//! selectors, not one: `BoxAnimated.glb` — this design's own canonical A1
//! fixture — animates translation on node 0 (an ancestor) and rotation on
//! node 2 (the mesh node itself), two DIFFERENT physical nodes for the
//! SAME object (`gltf_load::GltfObjectAnimation`'s `translation_node`/
//! `rotation_node`/`scale_node` fields, each independently `Option<usize>`
//! since a channel may not be animated at all). A single shared selector
//! silently samples the wrong node's data for whichever channel isn't at
//! that node — this was caught by the P2 gate's own visual re-render of
//! `BoxAnimated`, not assumed away. Sampling is a `partition_point` binary
//! search per channel (`gltf_anim_shared`'s slice samplers) instead of a
//! linear table scan.
//!
//! `progress` is port-shadowed like every other control-rate input in
//! this catalog: wire an `node.lfo` (Saw) or a fader for direct control,
//! or leave it unwired for the D3 default beat-drive —
//! `wrap(beats · rate / clip_beats)`, `clip_beats = duration_s ×
//! (beats-per-second implied by the current transport)`. Whichever the
//! source, the sampler always WRAPS `progress` into `[0, 1)` before
//! converting to a clip-relative time — never clamps — so a slightly
//! out-of-range value (an LFO edge, a scrub past the end) continues
//! smoothly into the next cycle instead of freezing at the boundary.
//!
//! LINEAR/STEP/CUBICSPLINE interpolation (whatever `gltf_anim_cache::
//! Channel::mode` carries): translation/scale lerp (or hold, or Hermite)
//! between the bracketing keyframes, rotation slerps (or holds, or
//! Hermite-blends+renormalizes) its quaternion pair then converts to the
//! XYZ Euler triple that reproduces the SAME rotation through
//! `render_scene::model_matrix`'s exact `Rz(z) · Ry_used(y) · Rx(x)`
//! composition (see [`quat_to_render_scene_euler`]). A channel absent
//! from the selected clip falls back to ITS OWN node selector's static
//! BIND pose (`GltfAnimSet::node_bind_trs`, the SAME never-identity-by-
//! default fallback `node.gltf_skeleton_pose`'s `sample_joint_local`
//! uses) — never fabricated motion. Nothing loaded yet (no `path`, or the
//! background load hasn't resolved) holds the output slots' existing
//! contents, the same convention `gltf_mesh_source`/`gltf_skeleton_pose`
//! use.

use std::borrow::Cow;
use std::sync::{Arc, mpsc};

use super::gltf_anim_shared::{
    LOOP_MODES, LoopMode, TriggerLatch, clip_duration, resolve_progress, sample_quat_slice, sample_vec3_slice,
};
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::gltf_anim_cache::{AnimSetLookup, GltfAnimSet, get_or_spawn_load};
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue, TableData};
use crate::node_graph::primitive::Primitive;

crate::primitive! {
    name: GltfAnimationSource,
    type_id: "node.gltf_animation_source",
    purpose: "Samples a parsed glTF TRS keyframe clip (read from the shared gltf_anim_cache, selected by path + per-channel translation_node/rotation_node/scale_node) at a live `progress` (0..1) and emits the nine pos_x/y/z, rot_x/y/z, scale_x/y/z scalars node.transform_3d's port-shadowed inputs accept — wire straight into an object's transform_3d to animate it. LINEAR/STEP/CUBICSPLINE interpolation (whatever the channel carries): lerp/hold/Hermite for translation/scale, slerp/hold/Hermite+renormalize then quat-to-Euler for rotation. `progress` port-shadowed: wire an LFO/fader for direct control, or leave unwired for the default beat-drive (wrap(beats*rate/clip_beats), clip_beats = duration_s scaled by the live transport) — always wraps into [0,1) before sampling, never clamps, so a clip loops continuously at the wrap point rather than freezing. A channel absent from the selected clip falls back to its own node selector's static bind pose, never a fabricated identity. Three independent node selectors (not one) because glTF assets legitimately split TRS channels across different ancestor nodes for one object (BoxAnimated.glb: translation on an ancestor, rotation on the mesh node).",
    inputs: {
        progress: ScalarF32 optional,
        clip_index: ScalarF32 optional,
        trigger_count: ScalarF32 optional,
    },
    outputs: {
        pos_x: ScalarF32,
        pos_y: ScalarF32,
        pos_z: ScalarF32,
        rot_x: ScalarF32,
        rot_y: ScalarF32,
        rot_z: ScalarF32,
        scale_x: ScalarF32,
        scale_y: ScalarF32,
        scale_z: ScalarF32,
    },
    params: [
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md D1/P2: comes via
        // presetMetadata.stringBindings, same convention as
        // node.gltf_mesh_source's/node.gltf_skeleton_pose's `path`.
        // Selects the `Arc<GltfAnimSet>` this node samples from
        // `gltf_anim_cache`'s shared cache.
        ParamDef {
            name: Cow::Borrowed("path"),
            label: "File",
            ty: ParamType::String,
            default: ParamValue::Float(0.0), // String default supplied via stringBindings; this slot is never read.
            range: None,
            enum_values: &[],
        },
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: one node-index selector PER
        // CHANNEL — see the module doc comment for why a single shared
        // selector is wrong (BoxAnimated.glb). `-1` (the default) means
        // "this object's channel is never animated in any clip" — the
        // sampler emits the static neutral default (0 pos/rot, 1 scale)
        // for it directly rather than attempting a bind-pose lookup.
        // Stamped by gltf_import.rs from `GltfObjectAnimation::
        // translation_node`/`rotation_node`/`scale_node`.
        ParamDef {
            name: Cow::Borrowed("translation_node"),
            label: "Translation Node",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 100_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rotation_node"),
            label: "Rotation Node",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 100_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("scale_node"),
            label: "Scale Node",
            ty: ParamType::Int,
            default: ParamValue::Float(-1.0),
            range: Some((-1.0, 100_000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("duration_s"),
            label: "Duration (s)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.001, 3600.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rate"),
            label: "Rate (cycles/clip)",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((0.0625, 16.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("clip_index"),
            label: "Clip",
            ty: ParamType::Int,
            default: ParamValue::Float(0.0),
            // GLTF_ANIM_RUNTIME_V2_DESIGN.md D6: follows the file, not an
            // arbitrary A4-era cap.
            range: Some((0.0, 255.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("loop_mode"),
            label: "Loop Mode",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, (LOOP_MODES.len() - 1) as f32)),
            enum_values: LOOP_MODES,
        },
        ParamDef {
            name: Cow::Borrowed("clip_durations"),
            label: "Clip Durations",
            ty: ParamType::Table,
            // Rows: [clip_index, duration_s]. Sentinel (unset) means "use
            // the static duration_s param for every clip" — the pre-A4
            // single-clip convention.
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("trigger_count"),
            label: "Retrigger",
            ty: ParamType::Trigger,
            // Port-shadowed by the same-named input (a graph trigger
            // source wins when wired); unwired, an outer-card `is_trigger`
            // button writes here directly (`ParamConvert::Trigger`'s
            // monotonic-counter convention) — no separate wire needed for
            // the card path. Trigger-typed (not Int) or card validation
            // rejects the import's Retrigger button.
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1_000_000.0)),
            enum_values: &[],
        },
        // GLTF_ANIMATION_DESIGN.md A1 fix (found rendering BoxAnimated.glb's
        // own goldens — the four phases came out pixel-identical): a
        // transform_3d input port is a port-SHADOW — wiring it makes the
        // wire win OUTRIGHT over the node's own static param, it does not
        // add to it. gltf_import.rs's per-object static recenter
        // (`-object_center`) lives on transform_3d's pos_x/y/z params, so
        // wiring this node's pos_x/y/z straight into those ports silently
        // discarded the recenter for every animated object — the sampled
        // translation landed in raw (un-recentered) glTF space instead of
        // the recentered space every OTHER node in the scene uses,
        // typically moving the object far outside the framing camera.
        // Composing the recenter HERE (added to the sampled position
        // before it reaches transform_3d) is the fix: one node still owns
        // the whole pos_x/y/z port-shadow, and its output is correct in
        // the same recentered space as a static object's.
        ParamDef {
            name: Cow::Borrowed("recenter_x"),
            label: "Recenter X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("recenter_y"),
            label: "Recenter Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("recenter_z"),
            label: "Recenter Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((-1000.0, 1000.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire `pos_x`..`scale_z` into a node.transform_3d's same-named input ports (its nine params are already port-shadowed — see docs/GLTF_ANIMATION_DESIGN.md A1). `path` comes via presetMetadata.stringBindings, same convention as node.gltf_mesh_source's `path`; `translation_node`/`rotation_node`/`scale_node` each independently select which scene node that ONE channel samples (gltf_import.rs stamps all four from GltfObjectAnimation) — `-1` means that channel is never animated for this object. Unwired `progress` follows the default beat-drive; wire node.lfo (Saw) for a performer-controlled loop, or any 0..1 scrub source.",
    examples: [],
    picker: { label: "glTF Animation Source", category: Driver },
    summary: "Plays back an imported glTF animation clip. Wire its outputs into a Transform 3D node to animate an imported object, or leave the progress input unwired to loop it on the beat.",
    category: Control,
    role: Source,
    aliases: ["gltf animation", "imported animation", "clip sampler", "keyframe sampler"],
    boundary_reason: NonGpu,
    extra_fields: {
        trigger_latch: TriggerLatch = TriggerLatch::new(),
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: same key-gated background-load
        // shape node.gltf_skeleton_pose's P1 rewire introduced.
        last_path: String = String::new(),
        anim_set: Option<Arc<GltfAnimSet>> = None,
        pending_load: Option<mpsc::Receiver<Result<Arc<GltfAnimSet>, String>>> = None,
    },
}

/// Quaternion `[x, y, z, w]` → the `(rot_x, rot_y, rot_z)` Euler triple
/// that reconstructs the SAME rotation through
/// `render_scene::model_matrix`'s exact composition order
/// (`euler_xyz_columns`: `Rz(z) · Ry(y) · Rx(x)`, column-major — see
/// that function's own array literals). The extraction formula below
/// (`rx = atan2(r21,r22)`, `ry = asin(-r20)`, `rz = atan2(r10,r00)`) was
/// derived and checked NUMERICALLY against that exact composition
/// (`euler_xyz_columns`'s literal `rx`/`ry`/`rz` arrays, transcribed
/// verbatim and multiplied out — a first hand-derivation attempt here,
/// working from a half-remembered "textbook" combined-matrix formula
/// instead of the actual source, got the signs wrong twice before this
/// numeric check caught it) and is re-verified in Rust by this module's
/// own round-trip test, which reconstructs `Rz·Ry·Rx` from the extracted
/// angles and checks it against the quaternion's own rotation matrix
/// bit-for-bit. Falls back to a `z = 0` decomposition at the gimbal-lock
/// singularity (`|r20| ~= 1`), the conventional resolution for this
/// Euler order — the fallback's sign also flips with the sign of `r20`
/// (verified the same way).
pub(crate) fn quat_to_render_scene_euler(q: [f32; 4]) -> [f32; 3] {
    let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
    let (xx, yy, zz) = (x * x, y * y, z * z);
    let (xy, xz, yz) = (x * y, x * z, y * z);
    let (wx, wy, wz) = (w * x, w * y, w * z);
    // Row-major rotation matrix r[row][col] — same convention as
    // `gltf_load::mat4_from_trs`'s upper-left 3x3 (that function is
    // column-major; these are its entries read row-major).
    let r20 = 2.0 * (xz - wy);
    let r21 = 2.0 * (yz + wx);
    let r22 = 1.0 - 2.0 * (xx + yy);
    let r10 = 2.0 * (xy + wz);
    let r00 = 1.0 - 2.0 * (yy + zz);
    let r01 = 2.0 * (xy - wz);
    let r11 = 1.0 - 2.0 * (xx + zz);

    let r20c = r20.clamp(-1.0, 1.0);
    if (1.0 - r20c.abs()) < 1e-6 {
        // Gimbal lock: x and z become degenerate around this axis (only
        // their sum/difference is recoverable). Pin z = 0 and fold the
        // whole rotation into x. `asin`'s derivative blows up at +/-1,
        // so computing `ry` via `asin(-r20c)` here would amplify the
        // f32 rounding already present in `r20c` into a much larger
        // angular error (measured: a quaternion built from
        // `sin/cos(PI/4)` alone put `r20c` ~6e-8 short of -1.0, which
        // `asin` turned into a ~3.5e-4 radian error) — since we already
        // know we're at the pole, set `ry` to the pole value directly
        // instead of trusting `asin` near its singularity.
        let ry = std::f32::consts::FRAC_PI_2.copysign(-r20c);
        let rx = if r20 < 0.0 { r01.atan2(r11) } else { (-r01).atan2(r11) };
        return [rx, ry, 0.0];
    }
    let rx = r21.atan2(r22);
    let ry = (-r20c).asin();
    let rz = r10.atan2(r00);
    [rx, ry, rz]
}

fn table_or_empty(v: Option<&ParamValue>) -> Option<&TableData> {
    match v {
        Some(ParamValue::Table(t)) => Some(t.as_ref()),
        _ => None,
    }
}

/// Sample each channel's LOCAL translation/rotation/scale at time `t`
/// within `clip` (looked up from `anim_set`), each against its OWN node
/// selector — see the module doc comment for why translation/rotation/
/// scale can legitimately come from three DIFFERENT scene nodes for one
/// object (`BoxAnimated.glb`). A `< 0` node (the "never animated"
/// sentinel) skips the bind-pose lookup entirely and returns the static
/// neutral default (0 pos/rot, 1 scale) directly. Otherwise falls back to
/// `anim_set.node_bind_trs[node]` when `clip` carries no track for that
/// node — the SAME never-fabricated-identity fallback
/// `gltf_skeleton_pose::sample_joint_local` uses; `clip: None` (an
/// out-of-range `clip_index`) samples every present-node channel from its
/// bind pose, same as a clip with no channels for that node at all. Pure
/// and `EffectNodeContext`-free so it's directly unit-testable.
pub(crate) fn sample_rigid_local(
    anim_set: &GltfAnimSet,
    clip: Option<&crate::node_graph::gltf_anim_cache::AnimClip>,
    translation_node: i32,
    rotation_node: i32,
    scale_node: i32,
    t: f32,
) -> ([f32; 3], [f32; 4], [f32; 3]) {
    let tr = if translation_node < 0 {
        [0.0, 0.0, 0.0]
    } else {
        let node = translation_node as u32;
        let bind = anim_set.node_bind_trs.get(node as usize).map(|b| b.translation).unwrap_or([0.0; 3]);
        clip.and_then(|c| c.translation_channel(node))
            .map(|c| sample_vec3_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t, bind))
            .unwrap_or(bind)
    };
    let rot = if rotation_node < 0 {
        [0.0, 0.0, 0.0, 1.0]
    } else {
        let node = rotation_node as u32;
        let bind = anim_set.node_bind_trs.get(node as usize).map(|b| b.rotation).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        clip.and_then(|c| c.rotation_channel(node))
            .map(|c| sample_quat_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t))
            .unwrap_or(bind)
    };
    let sc = if scale_node < 0 {
        [1.0, 1.0, 1.0]
    } else {
        let node = scale_node as u32;
        let bind = anim_set.node_bind_trs.get(node as usize).map(|b| b.scale).unwrap_or([1.0; 3]);
        clip.and_then(|c| c.scale_channel(node))
            .map(|c| sample_vec3_slice(&c.times, &c.values, c.mode, &c.in_tangents, &c.out_tangents, t, bind))
            .unwrap_or(bind)
    };
    (tr, rot, sc)
}

impl Primitive for GltfAnimationSource {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let duration_s_param = match ctx.params.get("duration_s") {
            Some(ParamValue::Float(f)) => f.max(1e-6),
            _ => 1.0,
        };
        let rate = match ctx.params.get("rate") {
            Some(ParamValue::Float(f)) => *f,
            _ => 1.0,
        };
        let loop_mode = LoopMode::from_enum_index(
            ctx.params.get("loop_mode").and_then(ParamValue::as_scalar).unwrap_or(0.0),
        );
        let clip_index = ctx
            .inputs
            .scalar("clip_index")
            .and_then(|v| v.as_scalar())
            .or_else(|| ctx.params.get("clip_index").and_then(ParamValue::as_scalar))
            .unwrap_or(0.0)
            .round()
            .max(0.0) as usize;

        let trigger_count = ctx
            .inputs
            .scalar("trigger_count")
            .and_then(|v| v.as_scalar())
            .or_else(|| ctx.params.get("trigger_count").and_then(ParamValue::as_scalar));
        self.trigger_latch.update(trigger_count, ctx.time.beats.0);

        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: `path` selects the shared
        // `Arc<GltfAnimSet>` — same key-gated background-load shape
        // node.gltf_skeleton_pose's P1 rewire introduced. Moved BEFORE
        // duration_s/progress so a resident clip's own `AnimClip::
        // duration_s` can drive playback speed. Each channel's OWN node
        // selector (see the module doc comment for why there are three,
        // not one) is read at sample time below.
        let path = match ctx.params.get("path") {
            Some(ParamValue::String(s)) => s.as_str().to_owned(),
            _ => String::new(),
        };
        let node_param = |name: &str| -> i32 {
            match ctx.params.get(name) {
                Some(ParamValue::Float(f)) => f.round() as i32,
                _ => -1,
            }
        };
        let translation_node = node_param("translation_node");
        let rotation_node = node_param("rotation_node");
        let scale_node = node_param("scale_node");

        if path != self.last_path {
            self.last_path = path.clone();
            self.anim_set = None;
            self.pending_load = None;
        }
        if self.anim_set.is_none() && self.pending_load.is_none() && !path.is_empty() {
            match get_or_spawn_load(std::path::Path::new(&path)) {
                AnimSetLookup::Ready(set) => self.anim_set = Some(set),
                AnimSetLookup::Pending(rx) => self.pending_load = Some(rx),
            }
        }
        if let Some(rx) = &self.pending_load {
            match rx.try_recv() {
                Ok(Ok(set)) => {
                    self.anim_set = Some(set);
                    self.pending_load = None;
                }
                Ok(Err(e)) => {
                    log::error!("node.gltf_animation_source: {e}");
                    self.pending_load = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    log::error!("node.gltf_animation_source: background load channel disconnected");
                    self.pending_load = None;
                }
            }
        }

        // Nothing loaded yet (or path empty/failed) — hold the output
        // slots' existing contents, same convention
        // `gltf_mesh_source`/`gltf_skeleton_pose` use.
        let Some(anim_set) = self.anim_set.clone() else {
            return;
        };

        let duration_s = match anim_set.clips.get(clip_index) {
            Some(c) => c.duration_s,
            None => {
                let clip_durations = table_or_empty(ctx.params.get("clip_durations"));
                clip_duration(clip_durations, clip_index, duration_s_param)
            }
        };

        let wired_progress = ctx.inputs.scalar("progress").and_then(|v| v.as_scalar());
        let progress = resolve_progress(
            ctx.time,
            wired_progress,
            duration_s,
            rate,
            loop_mode,
            self.trigger_latch.origin_beats(),
        );
        let t = progress * duration_s;

        let recenter = [
            ctx.params.get("recenter_x").and_then(ParamValue::as_scalar).unwrap_or(0.0),
            ctx.params.get("recenter_y").and_then(ParamValue::as_scalar).unwrap_or(0.0),
            ctx.params.get("recenter_z").and_then(ParamValue::as_scalar).unwrap_or(0.0),
        ];

        let clip = anim_set.clips.get(clip_index);
        let (sampled_pos, rot_quat, scale) =
            sample_rigid_local(&anim_set, clip, translation_node, rotation_node, scale_node, t);
        // Composed here, not left to the port-shadow at transform_3d — see
        // `recenter_x`'s doc comment.
        let pos = [
            sampled_pos[0] + recenter[0],
            sampled_pos[1] + recenter[1],
            sampled_pos[2] + recenter[2],
        ];
        let rot = quat_to_render_scene_euler(rot_quat);

        ctx.outputs.set_scalar("pos_x", ParamValue::Float(pos[0]));
        ctx.outputs.set_scalar("pos_y", ParamValue::Float(pos[1]));
        ctx.outputs.set_scalar("pos_z", ParamValue::Float(pos[2]));
        ctx.outputs.set_scalar("rot_x", ParamValue::Float(rot[0]));
        ctx.outputs.set_scalar("rot_y", ParamValue::Float(rot[1]));
        ctx.outputs.set_scalar("rot_z", ParamValue::Float(rot[2]));
        ctx.outputs.set_scalar("scale_x", ParamValue::Float(scale[0]));
        ctx.outputs.set_scalar("scale_y", ParamValue::Float(scale[1]));
        ctx.outputs.set_scalar("scale_z", ParamValue::Float(scale[2]));
    }

    fn clear_state(&mut self) {
        self.trigger_latch.clear();
    }

    fn is_trigger_latch(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::EffectNode;
    use crate::node_graph::MockBackend;
    use crate::node_graph::backend::Backend;
    use crate::node_graph::bindings::{NodeInputs, NodeOutputs, Slot};
    use crate::node_graph::effect_node::{FrameTime, ParamValues};
    use crate::node_graph::execution_plan::ResourceId;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;
    use manifold_core::{Beats, Seconds};
    use std::sync::Arc;

    fn frame_time(beats: f32, seconds: f32) -> FrameTime {
        FrameTime {
            beats: Beats(beats as f64),
            seconds: Seconds(seconds as f64),
            delta: Seconds(1.0 / 60.0),
            frame_count: 0,
        }
    }

    #[test]
    fn declares_progress_input_and_nine_scalar_outputs() {
        assert_eq!(GltfAnimationSource::TYPE_ID, "node.gltf_animation_source");
        assert_eq!(GltfAnimationSource::INPUTS.len(), 3);
        assert_eq!(GltfAnimationSource::INPUTS[0].name, "progress");
        assert!(!GltfAnimationSource::INPUTS[0].required);
        assert_eq!(GltfAnimationSource::INPUTS[1].name, "clip_index");
        assert_eq!(GltfAnimationSource::INPUTS[2].name, "trigger_count");
        assert_eq!(GltfAnimationSource::OUTPUTS.len(), 9);
        for (out, name) in GltfAnimationSource::OUTPUTS.iter().zip([
            "pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z",
        ]) {
            assert_eq!(out.name, name);
            assert_eq!(out.ty, PortType::Scalar(ScalarType::F32));
        }
    }

    #[test]
    fn primitive_registers_as_palette_atom() {
        let prim = GltfAnimationSource::new();
        let node: &dyn EffectNode = &prim;
        assert_eq!(node.type_id().as_str(), "node.gltf_animation_source");
    }

    fn translation_channel(node: u32, keys: &[(f32, [f32; 3])]) -> crate::node_graph::gltf_anim_cache::Channel {
        crate::node_graph::gltf_anim_cache::Channel {
            target_node: node,
            kind: crate::node_graph::gltf_anim_cache::ChannelKind::Translation,
            mode: crate::node_graph::gltf_load::GltfInterp::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: keys.iter().flat_map(|(_, v)| *v).collect(),
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }

    fn rotation_channel(node: u32, keys: &[(f32, [f32; 4])]) -> crate::node_graph::gltf_anim_cache::Channel {
        crate::node_graph::gltf_anim_cache::Channel {
            target_node: node,
            kind: crate::node_graph::gltf_anim_cache::ChannelKind::Rotation,
            mode: crate::node_graph::gltf_load::GltfInterp::Linear,
            times: keys.iter().map(|(t, _)| *t).collect(),
            values: keys.iter().flat_map(|(_, v)| *v).collect(),
            in_tangents: Vec::new(),
            out_tangents: Vec::new(),
        }
    }

    /// A single-clip `GltfAnimSet` with the given channels on node 0 and a
    /// generous `node_bind_trs` table (identity/1.0 for every node up to
    /// `node_count`) — the fixture shape every synthetic-cache test below
    /// shares.
    fn anim_set_one_clip(channels: Vec<crate::node_graph::gltf_anim_cache::Channel>, duration_s: f32, node_count: usize) -> GltfAnimSet {
        GltfAnimSet {
            clips: vec![crate::node_graph::gltf_anim_cache::AnimClip { duration_s, channels }],
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: vec![
                crate::node_graph::gltf_anim_cache::BindTrs {
                    translation: [0.0; 3],
                    rotation: [0.0, 0.0, 0.0, 1.0],
                    scale: [1.0; 3],
                };
                node_count
            ],
        }
    }

    /// Runs the primitive with a pre-loaded `anim_set` (bypassing the
    /// background-load machinery entirely — same technique
    /// `gltf_skeleton_pose`'s pure-fn tests use one level down, applied
    /// here at the `Primitive::run` level since this node's port surface
    /// IS the thing under test), the given params, and wired `progress`
    /// (`None` = unwired, default beat-drive). Returns the nine outputs in
    /// declaration order.
    fn run_with(
        anim_set: Option<GltfAnimSet>,
        overrides: &[(&'static str, ParamValue)],
        wired_progress: Option<f32>,
        time: FrameTime,
    ) -> [f32; 9] {
        let mut backend = MockBackend::new();
        let out_names =
            ["pos_x", "pos_y", "pos_z", "rot_x", "rot_y", "rot_z", "scale_x", "scale_y", "scale_z"];
        let out_slots: Vec<(&'static str, Slot)> = out_names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                (*name, backend.acquire(ResourceId(i as u32), PortType::Scalar(ScalarType::F32), None, (0, 0)))
            })
            .collect();

        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("duration_s"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("rate"), ParamValue::Float(1.0));
        // Every fixture channel in this test module targets node 0 —
        // stamp all three selectors to 0 by default so a test that only
        // cares about e.g. translation doesn't have to restate the other
        // two just to keep them out of the `-1` "never animated" sentinel.
        params.insert(Cow::Borrowed("translation_node"), ParamValue::Float(0.0));
        params.insert(Cow::Borrowed("rotation_node"), ParamValue::Float(0.0));
        params.insert(Cow::Borrowed("scale_node"), ParamValue::Float(0.0));
        for (name, value) in overrides {
            params.insert(Cow::Borrowed(*name), value.clone());
        }

        let mut wire_slots: Vec<(&'static str, Slot)> = Vec::new();
        if let Some(p) = wired_progress {
            let slot = backend.acquire(ResourceId(100), PortType::Scalar(ScalarType::F32), None, (0, 0));
            backend.set_scalar(slot, ParamValue::Float(p));
            wire_slots.push(("progress", slot));
        }

        let mut prim = GltfAnimationSource::new();
        // Bypass the background loader entirely: `path` param is left
        // unset (defaults to "" via ctx.params.get returning None), which
        // matches `last_path`'s own `String::new()` default, so `run()`'s
        // `path != self.last_path` check never fires and never resets
        // this pre-seeded `anim_set`.
        prim.anim_set = anim_set.map(Arc::new);

        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
        let outputs = NodeOutputs::new(
            &out_slots,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);

        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }

        let mut result = [0.0f32; 9];
        for (i, (_, slot)) in out_slots.iter().enumerate() {
            result[i] = match backend.scalar(*slot) {
                Some(ParamValue::Float(f)) => f,
                None => 0.0, // unwritten slot (no anim_set loaded) — MockBackend's zero-init default.
                other => panic!("expected Float on {}, got {other:?}", out_names[i]),
            };
        }
        result
    }

    #[test]
    fn no_anim_set_loaded_holds_the_outputs_pre_existing_contents() {
        // GLTF_ANIM_RUNTIME_V2_DESIGN.md P2: before a background load
        // resolves (or with no `path` at all), `run()` returns without
        // writing ANY output — a fresh slot's own zero-init, never a
        // fabricated "scale defaults to 1" write.
        let out = run_with(None, &[], Some(0.5), frame_time(0.0, 0.0));
        assert_eq!(out, [0.0; 9], "no anim_set resident -> no writes -> zero-init slots");
    }

    #[test]
    fn clip_with_no_channels_for_this_node_falls_back_to_bind_pose() {
        // An anim_set IS resident, but its one clip has zero channels for
        // node 0 (`target_node` default) — the SAME never-fabricated
        // bind-pose fallback `gltf_skeleton_pose::sample_joint_local`
        // proves, ported to the rigid path.
        let anim_set = anim_set_one_clip(Vec::new(), 1.0, 1);
        let out = run_with(Some(anim_set), &[], Some(0.5), frame_time(0.0, 0.0));
        assert_eq!(&out[0..6], &[0.0, 0.0, 0.0, 0.0, 0.0, 0.0], "pos/rot bind default to 0");
        assert_eq!(&out[6..9], &[1.0, 1.0, 1.0], "scale bind defaults to 1");
    }

    #[test]
    fn translation_channel_lerps_between_bracketing_keyframes() {
        let channel = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [10.0, 0.0, 0.0])]);
        let anim_set = anim_set_one_clip(vec![channel], 1.0, 1);
        // duration_s comes from the resident clip (1.0), progress = 0.5 ->
        // t = 0.5 -> halfway.
        let out = run_with(Some(anim_set), &[], Some(0.5), frame_time(0.0, 0.0));
        assert!((out[0] - 5.0).abs() < 1e-4, "pos_x should be halfway lerped, got {}", out[0]);
    }

    #[test]
    fn translation_channel_holds_boundary_values_outside_its_time_range() {
        // Keyframes only span [1.0, 2.0]; duration_s = 4.0 (the clip's own,
        // from the cache) so progress=0 (t=0) is before the first keyframe
        // and progress=0.99 (t=3.96) is after the last — both must hold
        // the boundary value, not extrapolate.
        let channel = translation_channel(0, &[(1.0, [5.0, 0.0, 0.0]), (2.0, [9.0, 0.0, 0.0])]);
        let anim_set = anim_set_one_clip(vec![channel], 4.0, 1);
        let before = run_with(Some(anim_set.clone()), &[], Some(0.0), frame_time(0.0, 0.0));
        assert!((before[0] - 5.0).abs() < 1e-4, "before first keyframe holds it, got {}", before[0]);
        let after = run_with(Some(anim_set), &[], Some(0.99), frame_time(0.0, 0.0));
        assert!((after[0] - 9.0).abs() < 1e-4, "after last keyframe holds it, got {}", after[0]);
    }

    #[test]
    fn progress_wraps_not_clamps_at_the_loop_boundary() {
        // A slightly-negative and slightly-over-1 progress must wrap to
        // the SAME point a plain in-range progress would reach — proving
        // the sampler uses rem_euclid, not clamp(0,1). If it clamped,
        // -0.01 would pin to t=0 (start-of-clip value) instead of
        // wrapping to ~0.99 (near-end-of-clip value).
        let channel = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [100.0, 0.0, 0.0])]);
        let anim_set = anim_set_one_clip(vec![channel], 1.0, 1);
        let wrapped_negative = run_with(Some(anim_set.clone()), &[], Some(-0.01), frame_time(0.0, 0.0));
        let plain_99 = run_with(Some(anim_set.clone()), &[], Some(0.99), frame_time(0.0, 0.0));
        assert!(
            (wrapped_negative[0] - plain_99[0]).abs() < 1e-3,
            "progress=-0.01 must wrap to ~0.99, not clamp to 0: got {} vs {}",
            wrapped_negative[0],
            plain_99[0]
        );

        let wrapped_over = run_with(Some(anim_set.clone()), &[], Some(1.01), frame_time(0.0, 0.0));
        let plain_01 = run_with(Some(anim_set), &[], Some(0.01), frame_time(0.0, 0.0));
        assert!(
            (wrapped_over[0] - plain_01[0]).abs() < 1e-3,
            "progress=1.01 must wrap to ~0.01, not clamp to 1: got {} vs {}",
            wrapped_over[0],
            plain_01[0]
        );
    }

    #[test]
    fn rotation_channel_slerps_and_holds_boundaries() {
        // 90-degree rotation about Z: quat(0,0,sin(45deg),cos(45deg)).
        let half = (std::f32::consts::FRAC_PI_4).sin();
        let cos_half = (std::f32::consts::FRAC_PI_4).cos();
        let channel = rotation_channel(0, &[(0.0, [0.0, 0.0, 0.0, 1.0]), (1.0, [0.0, 0.0, half, cos_half])]);
        let anim_set = anim_set_one_clip(vec![channel], 1.0, 1);
        let out = run_with(Some(anim_set), &[], Some(0.0), frame_time(0.0, 0.0));
        assert!(
            out[3].abs() < 1e-4 && out[4].abs() < 1e-4 && out[5].abs() < 1e-4,
            "identity quaternion should decode to zero Euler, got {:?}",
            &out[3..6]
        );
    }

    #[test]
    fn unwired_progress_follows_default_beat_drive() {
        // duration_s=1.0 (the clip's own), rate=1.0. At 120 BPM
        // (beats=2*seconds implied by the fallback ratio) progress =
        // beats / (duration_s * 2.0). beats=1.0 -> progress=0.5 -> t=0.5.
        let channel = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [8.0, 0.0, 0.0])]);
        let anim_set = anim_set_one_clip(vec![channel], 1.0, 1);
        let out = run_with(Some(anim_set), &[], None, frame_time(1.0, 0.5));
        assert!((out[0] - 4.0).abs() < 1e-3, "expected halfway through the clip, got {}", out[0]);
    }

    /// Numerically verifies [`quat_to_render_scene_euler`]'s derivation:
    /// composing the returned Euler triple through the SAME `Rz*Ry_used*Rx`
    /// formula `render_scene::model_matrix` uses must reproduce the
    /// original quaternion's own rotation matrix. Avoids exact gimbal-lock
    /// angles (|y| ~= 90 degrees) where the decomposition is ambiguous by
    /// construction.
    #[test]
    fn quat_to_euler_round_trips_through_render_scene_composition() {
        // Row-major r[row][col], matching `gltf_load::mat4_from_trs`'s
        // upper-left 3x3 EXACTLY (that function is column-major; this is
        // its transpose-to-row-major reading) — the authoritative
        // glTF-spec quat->matrix convention already load-bearing
        // elsewhere in this codebase.
        fn quat_to_matrix(q: [f32; 4]) -> [[f32; 3]; 3] {
            let (x, y, z, w) = (q[0], q[1], q[2], q[3]);
            let (xx, yy, zz) = (x * x, y * y, z * z);
            let (xy, xz, yz) = (x * y, x * z, y * z);
            let (wx, wy, wz) = (w * x, w * y, w * z);
            [
                [1.0 - 2.0 * (yy + zz), 2.0 * (xy - wz), 2.0 * (xz + wy)],
                [2.0 * (xy + wz), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - wx)],
                [2.0 * (xz - wy), 2.0 * (yz + wx), 1.0 - 2.0 * (xx + yy)],
            ]
        }
        // Same composition as render_scene::euler_xyz_columns, reproduced
        // here (that fn is private to its module) so this test verifies
        // the DERIVATION independent of a cross-module dependency.
        fn euler_xyz_columns(rot: [f32; 3]) -> [[f32; 3]; 3] {
            let (cx, sx) = (rot[0].cos(), rot[0].sin());
            let (cy, sy) = (rot[1].cos(), rot[1].sin());
            let (cz, sz) = (rot[2].cos(), rot[2].sin());
            let rx = [[1.0, 0.0, 0.0], [0.0, cx, sx], [0.0, -sx, cx]];
            let ry = [[cy, 0.0, -sy], [0.0, 1.0, 0.0], [sy, 0.0, cy]];
            let rz = [[cz, sz, 0.0], [-sz, cz, 0.0], [0.0, 0.0, 1.0]];
            fn mul(a: [[f32; 3]; 3], b: [[f32; 3]; 3]) -> [[f32; 3]; 3] {
                let mut out = [[0.0f32; 3]; 3];
                for col in 0..3 {
                    for row in 0..3 {
                        out[col][row] =
                            a[0][row] * b[col][0] + a[1][row] * b[col][1] + a[2][row] * b[col][2];
                    }
                }
                out
            }
            mul(mul(rz, ry), rx)
        }

        let half_90 = (std::f32::consts::FRAC_PI_4).sin(); // sin(45deg)
        let cos_45 = (std::f32::consts::FRAC_PI_4).cos();
        let general_raw = [0.2f32, 0.35, -0.15, 0.9];
        let general_len = general_raw.iter().map(|v| v * v).sum::<f32>().sqrt();
        let general = general_raw.map(|v| v / general_len);
        let cases: [[f32; 4]; 6] = [
            [0.0, 0.0, 0.0, 1.0],                                   // identity
            [0.0, 0.0, (0.4_f32).sin(), (0.4_f32).cos()],           // Z-only
            [(0.3_f32).sin(), 0.0, 0.0, (0.3_f32).cos()],           // X-only
            general,                                                // general
            [0.0, half_90, 0.0, cos_45],                            // gimbal lock: +90 deg about Y
            [0.0, -half_90, 0.0, cos_45],                           // gimbal lock: -90 deg about Y
        ];
        for q in cases {
            let euler = quat_to_render_scene_euler(q);
            // Row-major r[row][col] from the same standard quat formula,
            // in column-major array form (m[col][row]) to compare against
            // euler_xyz_columns' own column-major output.
            let rm = quat_to_matrix(q);
            let mut r_colmajor = [[0.0f32; 3]; 3];
            for row in 0..3 {
                for col in 0..3 {
                    r_colmajor[col][row] = rm[row][col];
                }
            }
            let reconstructed = euler_xyz_columns(euler);
            for col in 0..3 {
                for row in 0..3 {
                    assert!(
                        (r_colmajor[col][row] - reconstructed[col][row]).abs() < 1e-4,
                        "quat {q:?} -> euler {euler:?} did not reconstruct the same rotation \
                         at [{col}][{row}]: expected {}, got {}",
                        r_colmajor[col][row],
                        reconstructed[col][row]
                    );
                }
            }
        }
    }

    /// GLTF_ANIMATION_DESIGN.md A1's performer-gesture gate, at the
    /// GRAPH level: `node.lfo` (Saw, one cycle per beat) wired directly
    /// into `node.gltf_animation_source.progress`, sampled just before
    /// and just after the LFO's own wrap point (beats 0.999 and 1.001 —
    /// the LFO itself wraps `fract()` internally, already proven in
    /// `lfo.rs`'s tests). Uses a bounce-shaped translation track (rises
    /// then returns to 0 — the SAME shape `BoxAnimated.glb`'s real
    /// translation channel has, verified against the actual fixture
    /// bytes this session) so the two samples are expected to be close.
    ///
    /// NOTE for whoever reads this next to `BoxAnimated.glb` itself:
    /// that asset's ROTATION channel does NOT loop seamlessly (identity
    /// at t=0, held at 180°-about-X from t=2.5 to the clip's end at
    /// t=3.708 — verified against the raw keyframe bytes) — a real
    /// authored-content property of that specific fixture, not a
    /// sampler defect. A whole-frame pixel "near-progress-0 vs
    /// near-progress-1" render assertion would be FALSE for that asset;
    /// this test instead exercises the wrap-continuity claim on data
    /// that genuinely loops, at the value level (the reliable oracle
    /// for a computable claim), and `progress_wraps_not_clamps_at_the_loop_boundary`
    /// above independently proves the sampler's wrap-vs-clamp mechanics
    /// hold regardless of asset content.
    #[test]
    fn lfo_driven_progress_loops_a_seamless_track_continuously_at_the_wrap_point() {
        use crate::node_graph::EffectNode;
        use crate::node_graph::effect_node::EffectNodeType;
        use crate::node_graph::execution_plan::compile;
        use crate::node_graph::graph::Graph;
        use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType, ScalarType};
        use crate::node_graph::primitives::lfo::Lfo;
        use crate::node_graph::Executor;
        use manifold_core::{Beats, Seconds};
        use std::sync::Mutex;

        struct Capture {
            type_id: EffectNodeType,
            seen: std::sync::Arc<Mutex<Option<ParamValue>>>,
        }
        impl EffectNode for Capture {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
            fn type_id(&self) -> &EffectNodeType {
                &self.type_id
            }
            fn inputs(&self) -> &[NodeInput] {
                static INPUTS: [NodeInput; 1] = [NodePort {
                    name: Cow::Borrowed("in"),
                    ty: PortType::Scalar(ScalarType::F32),
                    kind: PortKind::Input,
                    required: true,
                }];
                &INPUTS
            }
            fn outputs(&self) -> &[NodeOutput] {
                &[]
            }
            fn parameters(&self) -> &[ParamDef] {
                &[]
            }
            fn evaluate(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
                *self.seen.lock().unwrap() = ctx.inputs.scalar("in");
            }
        }

        fn sample_pos_y_at(beats: f32) -> f32 {
            let seen = std::sync::Arc::new(Mutex::new(None));
            let mut g = Graph::new();
            let lfo = g.add_node(Box::new(Lfo::new()));
            g.set_param(lfo, "rate_mode", ParamValue::Enum(0)).unwrap(); // Musical
            g.set_param(lfo, "rate", ParamValue::Enum(0)).unwrap(); // "1/1" — one cycle per beat
            g.set_param(lfo, "shape", ParamValue::Enum(2)).unwrap(); // Saw

            let mut anim_prim = GltfAnimationSource::new();
            // Bounce shape matching BoxAnimated.glb's real translation
            // track: 0 -> peak -> peak (hold) -> back to 0. Pre-seeded
            // directly onto the primitive (bypassing the background
            // loader — same technique `run_with` uses) since this test
            // drives a real compiled Graph/Executor, not `run_with`'s
            // single-node harness.
            let channel = translation_channel(
                0,
                &[(0.0, [0.0, 2.52, 0.0]), (0.4, [0.0, 2.52, 0.0]), (1.0, [0.0, 0.0, 0.0])],
            );
            anim_prim.anim_set = Some(Arc::new(anim_set_one_clip(vec![channel], 1.0, 1)));
            let anim = g.add_node(Box::new(anim_prim));
            g.set_param(anim, "duration_s", ParamValue::Float(1.0)).unwrap();
            g.set_param(anim, "translation_node", ParamValue::Float(0.0)).unwrap();
            g.connect((lfo, "out"), (anim, "progress")).unwrap();

            let sink = g.add_node(Box::new(Capture {
                type_id: EffectNodeType::new("test.capture"),
                seen: seen.clone(),
            }));
            g.connect((anim, "pos_y"), (sink, "in")).unwrap();

            let plan = compile(&g).unwrap();
            let mut exec = Executor::with_mock();
            exec.execute_frame(
                &mut g,
                &plan,
                FrameTime {
                    beats: Beats(beats as f64),
                    seconds: Seconds(beats as f64 * 0.5),
                    delta: Seconds(1.0 / 60.0),
                    frame_count: 0,
                },
            );
            match seen.lock().unwrap().clone() {
                Some(ParamValue::Float(f)) => f,
                v => panic!("gltf_animation_source did not emit a Float on pos_y: {v:?}"),
            }
        }

        // rate=1/1 -> the LFO completes one Saw cycle per beat, so
        // beats=0.999 sits just before its own wrap and beats=1.001
        // just after (into the next cycle) — the LFO's `fract()` makes
        // these progress ~0.999 and ~0.001 respectively.
        let near_end = sample_pos_y_at(0.999);
        let near_start = sample_pos_y_at(1.001);
        assert!(
            (near_end - near_start).abs() < 0.05,
            "a seamless (0->peak->0) track must read continuously across the LFO's wrap: \
             near-end={near_end}, near-start={near_start}"
        );
    }

    /// GLTF_ANIMATION_DESIGN.md A4's performer-gesture gate: a `trigger_count`
    /// step (the value MIDI NoteOn drives via the phantom-clip path — see
    /// the phase brief's scope note) restarts the clip from `progress≈0`
    /// within one frame, on the default (unwired-`progress`) beat-drive path.
    /// Runs the SAME primitive instance across three frames so
    /// `trigger_latch` state persists, mirroring `clip_trigger_index`'s own
    /// test style rather than `run_with`'s fresh-instance-per-call shape.
    #[test]
    fn trigger_count_edge_retriggers_the_clip_from_zero_within_one_frame() {
        let channel = translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [100.0, 0.0, 0.0])]);
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("duration_s"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("rate"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("translation_node"), ParamValue::Float(0.0));

        let mut prim = GltfAnimationSource::new();
        prim.anim_set = Some(Arc::new(anim_set_one_clip(vec![channel], 1.0, 1)));
        let run_frame = |prim: &mut GltfAnimationSource, trigger: f32, time: FrameTime| -> f32 {
            let mut backend = MockBackend::new();
            let out_slot = backend.acquire(ResourceId(0), PortType::Scalar(ScalarType::F32), None, (0, 0));
            let trig_slot = backend.acquire(ResourceId(1), PortType::Scalar(ScalarType::F32), None, (0, 0));
            backend.set_scalar(trig_slot, ParamValue::Float(trigger));
            let wire_slots = [("trigger_count", trig_slot)];
            let out_slots = [("pos_x", out_slot)];
            let mut scalar_scratch = Vec::new();
            let mut camera_scratch = Vec::new();
            let mut light_scratch = Vec::new();
            let mut material_scratch = Vec::new();
            let mut transform_scratch = Vec::new();
            let mut atmosphere_scratch = Vec::new();
            let mut object_scratch = Vec::new();
            let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
            let outputs = NodeOutputs::new(
                &out_slots,
                &backend,
                &mut scalar_scratch,
                &mut camera_scratch,
                &mut light_scratch,
                &mut material_scratch,
                &mut transform_scratch,
                &mut atmosphere_scratch,
                &mut object_scratch,
            );
            let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
            Primitive::run(prim, &mut ctx);
            for (slot, value) in scalar_scratch.drain(..) {
                backend.set_scalar(slot, value);
            }
            match backend.scalar(out_slot) {
                Some(ParamValue::Float(f)) => f,
                other => panic!("expected Float on pos_x, got {other:?}"),
            }
        };

        // Frame 0 at beats=1.5 (duration_s=1, rate=1, 120bpm-equivalent
        // fallback ratio -> clip_beats=2 -> progress=0.75, mid-clip, far
        // from progress=0): establishes the trigger_count baseline without
        // firing.
        let deep = run_frame(&mut prim, 0.0, frame_time(1.5, 0.75));
        assert!((deep - 75.0).abs() < 1.0, "beats=1.5 -> progress=0.75 -> x=75 lerped: {deep}");

        // Frame 1, same beats, trigger_count edges 0 -> 1: must snap to
        // progress=0 WITHIN THIS FRAME (one-frame latency, not next-frame).
        let retriggered = run_frame(&mut prim, 1.0, frame_time(1.5, 0.75));
        assert!(
            (retriggered - 0.0).abs() < 1.0,
            "trigger_count edge must reset progress to 0 within one frame, got pos_x={retriggered}"
        );
    }

    #[test]
    fn clip_index_selects_between_independent_clips() {
        // Clip 0: 0 -> 10. Clip 1: 0 -> -10. Same time range, opposite sign
        // — two SEPARATE `AnimClip`s in the same `GltfAnimSet`, both
        // carrying a channel for node 0.
        let clip0 = crate::node_graph::gltf_anim_cache::AnimClip {
            duration_s: 1.0,
            channels: vec![translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [10.0, 0.0, 0.0])])],
        };
        let clip1 = crate::node_graph::gltf_anim_cache::AnimClip {
            duration_s: 1.0,
            channels: vec![translation_channel(0, &[(0.0, [0.0, 0.0, 0.0]), (1.0, [-10.0, 0.0, 0.0])])],
        };
        let anim_set = GltfAnimSet {
            clips: vec![clip0, clip1],
            skins: Vec::new(),
            node_parents: Vec::new(),
            node_bind_trs: vec![crate::node_graph::gltf_anim_cache::BindTrs {
                translation: [0.0; 3],
                rotation: [0.0, 0.0, 0.0, 1.0],
                scale: [1.0; 3],
            }],
        };

        let out_clip0 = run_with_clip(anim_set.clone(), 0.0, Some(0.5), frame_time(0.0, 0.0));
        let out_clip1 = run_with_clip(anim_set, 1.0, Some(0.5), frame_time(0.0, 0.0));
        assert!((out_clip0 - 5.0).abs() < 1e-3, "clip 0 halfway: got {out_clip0}");
        assert!((out_clip1 - -5.0).abs() < 1e-3, "clip 1 halfway: got {out_clip1}");
    }

    /// Runs a single frame with a pre-loaded `anim_set` and a wired
    /// `clip_index`/`progress`, sharing `run_with`'s bypass-the-loader
    /// technique but returning just `pos_x` (the `clip_index_selects_...`
    /// test only needs one scalar to prove clip selection).
    fn run_with_clip(anim_set: GltfAnimSet, clip_index: f32, progress: Option<f32>, time: FrameTime) -> f32 {
        let mut backend = MockBackend::new();
        let out_slot = backend.acquire(ResourceId(0), PortType::Scalar(ScalarType::F32), None, (0, 0));
        let clip_slot = backend.acquire(ResourceId(1), PortType::Scalar(ScalarType::F32), None, (0, 0));
        backend.set_scalar(clip_slot, ParamValue::Float(clip_index));
        let mut wire_slots = vec![("clip_index", clip_slot)];
        let progress_slot;
        if let Some(p) = progress {
            progress_slot = backend.acquire(ResourceId(2), PortType::Scalar(ScalarType::F32), None, (0, 0));
            backend.set_scalar(progress_slot, ParamValue::Float(p));
            wire_slots.push(("progress", progress_slot));
        }
        let out_slots = [("pos_x", out_slot)];
        let mut params = ParamValues::default();
        params.insert(Cow::Borrowed("duration_s"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("rate"), ParamValue::Float(1.0));
        params.insert(Cow::Borrowed("translation_node"), ParamValue::Float(0.0));
        let mut scalar_scratch = Vec::new();
        let mut camera_scratch = Vec::new();
        let mut light_scratch = Vec::new();
        let mut material_scratch = Vec::new();
        let mut transform_scratch = Vec::new();
        let mut atmosphere_scratch = Vec::new();
        let mut object_scratch = Vec::new();
        let inputs = NodeInputs::new(&wire_slots, &backend, &[]);
        let outputs = NodeOutputs::new(
            &out_slots,
            &backend,
            &mut scalar_scratch,
            &mut camera_scratch,
            &mut light_scratch,
            &mut material_scratch,
            &mut transform_scratch,
            &mut atmosphere_scratch,
            &mut object_scratch,
        );
        let mut prim = GltfAnimationSource::new();
        prim.anim_set = Some(Arc::new(anim_set));
        let mut ctx = EffectNodeContext::new(time, &params, inputs, outputs, None);
        Primitive::run(&mut prim, &mut ctx);
        for (slot, value) in scalar_scratch.drain(..) {
            backend.set_scalar(slot, value);
        }
        match backend.scalar(out_slot) {
            Some(ParamValue::Float(f)) => f,
            other => panic!("expected Float on pos_x, got {other:?}"),
        }
    }

    #[test]
    fn is_trigger_latch_flag_is_set() {
        let prim = GltfAnimationSource::new();
        let node: &dyn EffectNode = &prim;
        assert!(node.is_trigger_latch());
    }

    #[test]
    fn clear_state_releases_the_trigger_latch() {
        let mut prim = GltfAnimationSource::new();
        prim.trigger_latch.update(Some(0.0), 0.0);
        prim.trigger_latch.update(Some(1.0), 10.0);
        assert_eq!(prim.trigger_latch.origin_beats(), Some(10.0));
        {
            let node: &mut dyn EffectNode = &mut prim;
            node.clear_state();
        }
        assert_eq!(prim.trigger_latch.origin_beats(), None);
    }
}
