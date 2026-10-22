//! `node.curve_solid_source` — the solid a curve sweep draws, as a
//! `MeshSource` for collision shapes and copy layouts.
//!
//! A sweep (`node.extrude_curve`, `node.revolve_curve`, `node.tube_from_path`
//! into `node.make_triangles`) builds its mesh on the GPU, where a collision
//! shape cannot read it. This node reads the same curve points on the CPU
//! (they come from CPU curve atoms such as `node.combine_xy`) and publishes
//! `MeshSource::Swept`, whose CPU mirror (`curve_sweep`) is the sweep's
//! triangles. Its params carry the sweep's own names, so a compiler mirrors
//! the sweep node's params and live rows onto it.
//!
//! The published solid is reused while the curve and the sweep stay the
//! same, so a still curve allocates nothing per frame and a rigid body sees
//! the same solid; a moved row publishes a new one, and the body rebuilds its
//! shape without restarting its world.

use std::borrow::Cow;
use std::f32::consts::TAU;
use std::sync::Arc;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::CurvePoint;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::scene::curve_sweep::{Sweep, SweptCurve};
use manifold_node_engine::scene::mesh_source::MeshSource;

const fn flag(name: &'static str, label: &'static str) -> ParamDef {
    ParamDef { name: Cow::Borrowed(name), label, ty: ParamType::Bool, default: ParamValue::Bool(false), range: None, enum_values: &[] }
}

manifold_node_engine::primitive! {
    name: CurveSolidSource,
    type_id: "node.curve_solid_source",
    purpose: "Publish the solid a curve sweep draws as a MeshSource: `kind` Extrude, Revolve or Tube reads `curve` (and a Tube's `lift`) on the CPU with the matching sweep node's params, and the source's vertices are that sweep followed by node.make_triangles, so a rigid body or a copy layout gets the drawn shape. Extrude reads depth, steps, close, symmetric and caps; Revolve reads segments and sweep; Tube reads radius, sides, caps and lift.",
    inputs: {
        curve: Array(CurvePoint) required,
        lift: Array(f32) optional,
        depth: ScalarF32 optional,
        sweep: ScalarF32 optional,
        radius: ScalarF32 optional,
    },
    outputs: {
        source: MeshSource,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("kind"),
            label: "Sweep",
            ty: ParamType::Enum,
            default: ParamValue::Enum(0),
            range: Some((0.0, 2.0)),
            enum_values: &["Extrude", "Revolve", "Tube"],
        },
        ParamDef {
            name: Cow::Borrowed("depth"),
            label: "Depth",
            ty: ParamType::Float,
            default: ParamValue::Float(1.0),
            range: Some((-100.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("steps"),
            label: "Steps",
            ty: ParamType::Int,
            default: ParamValue::Float(1.0),
            range: Some((1.0, 256.0)),
            enum_values: &[],
        },
        flag("close", "Close"),
        flag("symmetric", "Symmetric"),
        flag("caps", "Fill Caps"),
        ParamDef {
            name: Cow::Borrowed("segments"),
            label: "Segments",
            ty: ParamType::Int,
            default: ParamValue::Float(48.0),
            range: Some((1.0, 512.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sweep"),
            label: "Sweep",
            ty: ParamType::Float,
            default: ParamValue::Float(TAU),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.1),
            range: Some((0.0, 100.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("sides"),
            label: "Sides",
            ty: ParamType::Int,
            default: ParamValue::Float(8.0),
            range: Some((3.0, 64.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire the same curve the sweep reads into `curve`, give it the sweep's params (a scene compiler mirrors them, live rows included), and wire `source` into node.rigid_body's source or mesh_k. The curve must come from CPU atoms (node.range, node.array_math, node.polygon_curve, node.combine_xy): a GPU-written curve is not readable here the same frame. The MeshSource is the sweep with node.make_triangles' Keep Normals, minus the zero-area triangles at a doubled rim.",
    examples: [],
    picker: { label: "Curve Solid", category: Atom },
    summary: "Hands the solid an extruded, revolved or tube curve makes to physics and copies, so a curve shape can fall, collide or be copied.",
    category: Geometry3D,
    role: Source,
    aliases: ["curve solid", "curve collider", "swept mesh source"],
    boundary_reason: NonGpu,
    extra_fields: {
        // The last solid published, reused while nothing changes.
        published: Option<Arc<SweptCurve>> = None,
    },
}

fn count(ctx: &EffectNodeContext<'_, '_>, name: &str, default: u32, min: f32) -> u32 {
    match ctx.params.get(name) {
        Some(ParamValue::Float(n)) if n.is_finite() => n.round().max(min) as u32,
        _ => default,
    }
}

impl Primitive for CurveSolidSource {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let on = |name: &str| matches!(ctx.params.get(name), Some(ParamValue::Bool(true)));
        let kind = match ctx.params.get("kind") {
            Some(ParamValue::Enum(v)) => *v,
            Some(ParamValue::Float(v)) => v.round().max(0.0) as u32,
            _ => 0,
        };
        let sweep = match kind {
            0 => Sweep::Extrude {
                depth: ctx.scalar_or_param("depth", 1.0),
                steps: count(ctx, "steps", 1, 1.0),
                close: on("close"),
                symmetric: on("symmetric"),
                caps: on("caps"),
            },
            1 => Sweep::Revolve { segments: count(ctx, "segments", 48, 1.0), sweep: ctx.scalar_or_param("sweep", TAU) },
            _ => Sweep::Tube { radius: ctx.scalar_or_param("radius", 0.1), sides: count(ctx, "sides", 8, 3.0), caps: on("caps") },
        };
        let Some(curve) = ctx.inputs.array("curve").and_then(|buf| {
            let ptr = buf.mapped_ptr()?;
            let len = (buf.size / std::mem::size_of::<CurvePoint>() as u64) as usize;
            // Safety: shared-memory buffer the CPU curve atom upstream wrote
            // this frame; the executor runs one node at a time on the
            // content thread.
            Some(unsafe { std::slice::from_raw_parts(ptr as *const [f32; 2], len) })
        }) else {
            return;
        };
        let lift: &[f32] = match (&sweep, ctx.inputs.array("lift")) {
            (Sweep::Tube { .. }, Some(buf)) => match buf.mapped_ptr() {
                // Safety: as for `curve`.
                Some(ptr) => unsafe { std::slice::from_raw_parts(ptr as *const f32, (buf.size / 4) as usize) },
                None => &[],
            },
            _ => &[],
        };
        let unchanged = self.published.as_ref().is_some_and(|solid| solid.is(curve, lift, sweep));
        if !unchanged {
            self.published = Some(Arc::new(SweptCurve { points: curve.to_vec(), lift: lift.to_vec(), sweep }));
        }
        if let Some(solid) = &self.published {
            ctx.outputs.set_mesh_source("source", MeshSource::Swept(Arc::clone(solid)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::polygon_curve::outline_point;
    use super::*;
    use std::collections::HashMap;

    /// Edges no other triangle shares: zero for a closed solid. Corners weld
    /// at 0.1 mm, so a seam that only nearly meets still counts as closed.
    fn open_edges(vertices: &[manifold_node_engine::mesh::MeshVertex]) -> usize {
        let key = |p: [f32; 3]| p.map(|v| (v * 1e4).round() as i64);
        let mut edges: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
        for tri in vertices.chunks(3) {
            for (a, b) in [(0, 1), (1, 2), (2, 0)] {
                let (a, b) = (key(tri[a].position), key(tri[b].position));
                *edges.entry(if a < b { (a, b) } else { (b, a) }).or_default() += 1;
            }
        }
        edges.values().filter(|&&uses| uses != 2).count()
    }

    fn bounds(vertices: &[manifold_node_engine::mesh::MeshVertex]) -> ([f32; 3], [f32; 3]) {
        vertices.iter().fold(([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]), |(lo, hi), v| {
            (std::array::from_fn(|k| lo[k].min(v.position[k])), std::array::from_fn(|k| hi[k].max(v.position[k])))
        })
    }

    fn near(a: [f32; 3], b: [f32; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-3)
    }

    /// The solid a rigid body or a copy layout gets from each sweep of a
    /// Polygon Curve hexagon is closed, and its hull is the shape drawn:
    /// the hexagon pushed to its depth, spun into a ring, or a tube along an
    /// arc with its ends capped.
    #[test]
    fn each_sweep_of_a_polygon_is_a_closed_solid_with_its_hull() {
        let (count, radius) = (241, 0.5);
        let hexagon: Vec<[f32; 2]> = (0..count).map(|i| outline_point(i, count, 6.0, radius, 0.0)).collect();
        // Flat sides at x = ±r cos 30°, corners at y = ±r.
        let half_width = radius * (std::f32::consts::PI / 6.0).cos();
        let extrude = SweptCurve {
            points: hexagon.clone(),
            lift: Vec::new(),
            sweep: Sweep::Extrude { depth: 2.0, steps: 3, close: false, symmetric: false, caps: true },
        };
        // The hexagon one and a half metres off the axis, spun a full turn.
        let profile: Vec<[f32; 2]> = hexagon.iter().map(|[x, y]| [x + 1.5, *y]).collect();
        let revolve = SweptCurve { points: profile, lift: Vec::new(), sweep: Sweep::Revolve { segments: 48, sweep: TAU } };
        let arc: Vec<[f32; 2]> = (0..17).map(|i| (std::f32::consts::PI * i as f32 / 16.0).sin_cos()).map(|(s, c)| [2.0 * c, 2.0 * s]).collect();
        let tube = SweptCurve { points: arc, lift: Vec::new(), sweep: Sweep::Tube { radius: 0.25, sides: 12, caps: true } };
        let outer = 1.5 + half_width;
        // An end ring faces along the arc's first chord, tilted by half a
        // step (pi / 32), so it reaches a little past the arc's end plane.
        let tilt = std::f32::consts::PI / 32.0;
        let (end_x, end_z) = (2.0 + 0.25 * tilt.cos(), -0.25 * tilt.sin());
        for (swept, lo, hi) in [
            (&extrude, [-half_width, -radius, 0.0], [half_width, radius, 2.0]),
            (&revolve, [-outer, -radius, -outer], [outer, radius, outer]),
            (&tube, [-end_x, -0.25, end_z], [end_x, 0.25, 2.25]),
        ] {
            let vertices = swept.load_vertices().expect("the sweep has area");
            assert_eq!(open_edges(&vertices), 0, "{:?} leaves a hole", swept.sweep);
            let (got_lo, got_hi) = bounds(&vertices);
            assert!(near(got_lo, lo) && near(got_hi, hi), "{:?}: hull {got_lo:?}..{got_hi:?}, want {lo:?}..{hi:?}", swept.sweep);
        }
    }
}
