//! `node.polygon_curve` — the outline of a regular polygon or star as two
//! `Array<f32>` channels (x, y), sampled at a fixed count so the number of
//! sides can move live without resizing anything downstream.
//!
//! The outline is the closed polyline through 2N corners: the N outer corners
//! at `radius`, and between each pair an inner corner at
//! `radius · cos(π/N) · (1 − pinch)`. Pinch 0 puts every inner corner on the
//! straight edge, so the outline is the regular N-gon; above 0 it is an
//! N-pointed star. Sample `i` of `count` (end-inclusive, so the last sample
//! closes on the first) walks the corners at even spacing, starting at the
//! corner straight up (+Y).
//!
//! Curve geometry needs straight edges with a live corner count: atoms can
//! only do that with a periodic op `node.array_math` lacks and about thirty
//! nodes for a star, so this is one curated source, the 2D sibling of
//! `node.polytope_vertices`.
//!
//! CPU-only, like `node.range`: a few hundred samples, written to shared
//! memory that downstream CPU curve atoms read the same frame.

use std::borrow::Cow;
use std::f32::consts::{FRAC_PI_2, PI};

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;

/// The largest sample count one frame writes (the `max_capacity` range): the
/// stack scratch holds one channel.
const SCRATCH_LEN: usize = 4096;

manifold_node_engine::primitive! {
    name: PolygonCurve,
    type_id: "node.polygon_curve",
    purpose: "Emit the outline of a regular polygon (pinch 0) or an N-pointed star (pinch above 0) as two Array<f32> channels x and y, `max_capacity` samples spaced evenly along the outline, end-inclusive so the last sample closes on the first. Sides rounds to a whole number and can move live: the sample count never changes.",
    inputs: {
        sides: ScalarF32 optional,
        radius: ScalarF32 optional,
        pinch: ScalarF32 optional,
    },
    outputs: {
        x: Array(f32),
        y: Array(f32),
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("sides"),
            label: "Sides",
            ty: ParamType::Float,
            default: ParamValue::Float(6.0),
            range: Some((3.0, 32.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("radius"),
            label: "Radius",
            ty: ParamType::Float,
            default: ParamValue::Float(0.5),
            range: Some((0.0, 10.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("pinch"),
            label: "Pinch",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("max_capacity"),
            label: "Sample Count",
            ty: ParamType::Int,
            default: ParamValue::Float(241.0),
            range: Some((4.0, 4096.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Pair with node.combine_xy to make an Array<CurvePoint> for node.extrude_curve, node.revolve_curve, node.tube_from_path or node.draw_lines. A count of 1 + a multiple of 2·sides lands a sample on every corner (241 does for 3, 4, 5, 6, 8, 10 and 12 sides). Drive `sides` from a counter for a shape that steps through polygons; drive `pinch` for a polygon that grows into a star.",
    examples: [],
    picker: { label: "Polygon Curve", category: Atom },
    summary: "Draws the outline of a polygon or a star as a list of points. Sides sets the corners; Pinch pulls every other corner in to make a star.",
    category: Geometry3D,
    role: Source,
    aliases: ["polygon curve", "polygon", "star", "ngon", "outline"],
    boundary_reason: NonGpu,
}

/// Sample `i` of `count` on the outline with `sides` corners.
pub(crate) fn outline_point(i: usize, count: usize, sides: f32, radius: f32, pinch: f32) -> [f32; 2] {
    let t = i as f32 / (count.max(2) - 1) as f32;
    let u = t * 2.0 * sides;
    let corner = u.floor();
    let along = u - corner;
    let inner = radius * (PI / sides).cos() * (1.0 - pinch);
    let at = |k: f32| {
        let r = if (k as i64) % 2 == 0 { radius } else { inner };
        let angle = FRAC_PI_2 + k * PI / sides;
        [r * angle.cos(), r * angle.sin()]
    };
    let a = at(corner);
    let b = at(corner + 1.0);
    [a[0] + (b[0] - a[0]) * along, a[1] + (b[1] - a[1]) * along]
}

impl Primitive for PolygonCurve {
    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let sides = ctx.scalar_or_param("sides", 6.0).round().max(3.0);
        let radius = ctx.scalar_or_param("radius", 0.5);
        let pinch = ctx.scalar_or_param("pinch", 0.0).clamp(0.0, 1.0);
        let (Some(x_buf), Some(y_buf)) = (ctx.outputs.array("x"), ctx.outputs.array("y")) else {
            return;
        };
        let f32_size = std::mem::size_of::<f32>() as u64;
        let count = ((x_buf.size / f32_size).min(y_buf.size / f32_size) as usize).min(SCRATCH_LEN);
        if count == 0 {
            return;
        }
        let mut xs = [0.0_f32; SCRATCH_LEN];
        let mut ys = [0.0_f32; SCRATCH_LEN];
        for i in 0..count {
            let [x, y] = outline_point(i, count, sides, radius, pinch);
            xs[i] = x;
            ys[i] = y;
        }
        // Safety: shared-memory buffers pre-bound by the chain build, writes
        // clamped to their capacity; the executor runs one node at a time on
        // the content thread.
        unsafe {
            x_buf.write(0, bytemuck::cast_slice(&xs[..count]));
            y_buf.write(0, bytemuck::cast_slice(&ys[..count]));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::outline_point;
    use manifold_node_engine::scene::curve_sweep::is_loop;
    use std::f32::consts::{FRAC_PI_2, PI};

    /// The outline closes on its start, its outer corners sit on the radius
    /// where a regular polygon's do, and nothing reaches past them: the hull
    /// is the polygon. A pinched star's inner corners sit where Pinch puts them.
    #[test]
    fn the_outline_is_closed_and_its_hull_is_the_polygon() {
        let (count, sides, radius) = (241, 6.0, 0.5);
        for pinch in [0.0, 0.5] {
            let points: Vec<[f32; 2]> = (0..count).map(|i| outline_point(i, count, sides, radius, pinch)).collect();
            assert!(is_loop(&points), "pinch {pinch}: the outline ends where it starts");
            let inner = radius * (PI / sides).cos() * (1.0 - pinch);
            // 241 samples land on every corner: corner c at sample 20c.
            for corner in 0..12 {
                let [x, y] = points[corner * 20];
                let angle = FRAC_PI_2 + corner as f32 * PI / sides;
                let r = if corner % 2 == 0 { radius } else { inner };
                assert!((x - r * angle.cos()).abs() < 1e-5 && (y - r * angle.sin()).abs() < 1e-5, "pinch {pinch}: corner {corner} at {x}, {y}");
            }
            assert!(points.iter().all(|[x, y]| (x * x + y * y).sqrt() <= radius + 1e-5), "pinch {pinch}: a sample is past the outer corners");
        }
    }
}
