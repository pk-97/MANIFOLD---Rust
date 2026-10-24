//! Curve shapes (Extrude, Revolve, Tube): a built-in curve made of curve
//! atoms, packed into points (`node.combine_xy`), swept into a grid by
//! `node.extrude_curve`, `node.revolve_curve` or `node.tube_from_path`, and
//! turned into triangles by `node.make_triangles`. Every curve row is a card
//! param on one of these nodes (`manifold_core::scene_schema` curves table),
//! so a row moves the solid on the next frame with no recompile.
//!
//! Curves lie in the shape's own XY plane for Extrude and Revolve (Revolve
//! spins about Y, the curve shifted off the axis by Offset) and in the XZ
//! ground plane for Tube. Helix is the one curve that rises: a Tube follows
//! it up, and Extrude and Revolve take its side view.

use std::f32::consts::TAU;

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_core::scene_doc::{Curve, Item, Shape};
use manifold_core::scene_schema::{
    self, CURVE_AXIS, CURVE_MATH, CURVE_POLYGON, CURVE_RATIO, CURVE_T, CURVE_XY, SOLID, SWEEP,
    TRIANGLES,
};

use super::builder::{Builder, Rest};

/// Samples along every curve: one plus a multiple of 2·sides for 3, 4, 5,
/// 6, 8, 10 and 12 sides, so a polygon's corners land on samples.
const SAMPLES: u32 = 241;
/// Rows along an extrusion's depth, so a bend along it has vertices to move.
const EXTRUDE_STEPS: u32 = 8;
const REVOLVE_SEGMENTS: u32 = 64;
const TUBE_SIDES: u32 = 16;
/// `node.combine_xy` scales points by a quarter for line drawing; this undoes
/// it, so curve points come out in metres.
const UNIT_POINTS: f32 = 4.0;

/// `node.array_math` ops.
const MULTIPLY: f32 = 2.0;
const SCALE_OFFSET: f32 = 6.0;
const SIN: f32 = 11.0;
const COS: f32 = 12.0;

type Port = (u32, &'static str);

/// What a curve shape draws: the solid physics and copies read (a node with
/// a `source` output), the triangles the draw reads, and the rest pose at
/// the kind's defaults.
pub(crate) struct CurveMesh {
    pub solid: u32,
    pub vertices: Port,
    pub rest: Option<Rest>,
}

/// A curve's channels: x and y in its plane, and Helix's rise.
struct Channels {
    x: Port,
    y: Port,
    rise: Option<Port>,
}

/// The emitting context for one item's curve.
struct Emit<'b, 'd> {
    b: &'b mut Builder<'d>,
    owner: String,
}

impl Emit<'_, '_> {
    fn range(&mut self, start: f32, end: f32) -> Port {
        let t = self.b.add(&self.owner, CURVE_T, "node.range");
        self.b.set_number(t, "start", start);
        self.b.set_number(t, "end", end);
        self.b.set_number(t, "max_capacity", SAMPLES as f32);
        self.b.set_number(t, "end_inclusive", 1.0);
        (t, "out")
    }

    /// `node.array_math` number `k` of the curve: `op` on `a` (and `with`),
    /// at `scale` and `offset`.
    fn math(
        &mut self,
        k: usize,
        op: f32,
        a: Port,
        with: Option<Port>,
        scale: f32,
        offset: f32,
    ) -> Port {
        let node = self.b.add(&self.owner, CURVE_MATH[k], "node.array_math");
        self.b.set_number(node, "op", op);
        self.b.set_number(node, "scale", scale);
        self.b.set_number(node, "offset", offset);
        self.b.wire(a.0, a.1, node, "a");
        if let Some(with) = with {
            self.b.wire(with.0, with.1, node, "b");
        }
        (node, "out")
    }

    fn scale(&mut self, k: usize, a: Port, scale: f32) -> Port {
        self.math(k, SCALE_OFFSET, a, None, scale, 0.0)
    }

    fn curve(&mut self, curve: Curve) -> Channels {
        match curve {
            Curve::Circle => {
                let t = self.range(0.0, TAU);
                let c = self.math(0, COS, t, None, 1.0, 0.0);
                let s = self.math(1, SIN, t, None, 1.0, 0.0);
                let x = self.scale(2, c, 1.0);
                let y = self.scale(3, s, 1.0);
                Channels { x, y, rise: None }
            }
            Curve::Polygon | Curve::Star => {
                let outline = self.b.add(&self.owner, CURVE_POLYGON, "node.polygon_curve");
                self.b.set_number(outline, "max_capacity", SAMPLES as f32);
                Channels {
                    x: (outline, "x"),
                    y: (outline, "y"),
                    rise: None,
                }
            }
            Curve::Spiral => {
                let t = self.range(0.0, 1.0);
                let turns = self.scale(0, t, 1.0);
                let angle = self.scale(1, turns, TAU);
                let r = self.scale(2, t, 1.0);
                let c = self.math(3, COS, angle, None, 1.0, 0.0);
                let s = self.math(4, SIN, angle, None, 1.0, 0.0);
                let x = self.math(5, MULTIPLY, r, Some(c), 1.0, 0.0);
                let y = self.math(6, MULTIPLY, r, Some(s), 1.0, 0.0);
                Channels { x, y, rise: None }
            }
            Curve::Helix => {
                let t = self.range(-0.5, 0.5);
                let turns = self.scale(0, t, 1.0);
                let angle = self.scale(1, turns, TAU);
                let c = self.math(2, COS, angle, None, 1.0, 0.0);
                let s = self.math(3, SIN, angle, None, 1.0, 0.0);
                let x = self.scale(4, c, 1.0);
                let y = self.scale(5, s, 1.0);
                let rise = self.scale(6, t, 1.0);
                Channels {
                    x,
                    y,
                    rise: Some(rise),
                }
            }
            Curve::Wave => {
                let t = self.range(-0.5, 0.5);
                let y = self.scale(0, t, 1.0);
                let waves = self.scale(1, t, 1.0);
                let angle = self.scale(2, waves, TAU);
                let s = self.math(3, SIN, angle, None, 1.0, 0.0);
                let x = self.scale(4, s, 1.0);
                Channels { x, y, rise: None }
            }
            Curve::Lissajous => {
                let t = self.range(0.0, TAU);
                let ratio = self.b.add(&self.owner, CURVE_RATIO, "node.frequency_ratio");
                let ax = self.scale(0, t, 1.0);
                self.b.wire(ratio, "a", ax.0, "scale");
                let sx = self.math(1, SIN, ax, None, 1.0, 0.0);
                let ay = self.scale(2, t, 1.0);
                self.b.wire(ratio, "b", ay.0, "scale");
                let sy = self.math(3, SIN, ay, None, 1.0, 0.0);
                let x = self.scale(4, sx, 1.0);
                let y = self.scale(5, sy, 1.0);
                Channels { x, y, rise: None }
            }
        }
    }

    fn points(&mut self, x: Port, y: Port) -> u32 {
        let xy = self.b.add(&self.owner, CURVE_XY, "node.combine_xy");
        self.b.set_number(xy, "scale", UNIT_POINTS);
        self.b.wire(x.0, x.1, xy, "x");
        self.b.wire(y.0, y.1, xy, "y");
        xy
    }

    fn triangles(&mut self, grid: u32, cols: u32, rows: u32) -> Port {
        let tri = self.b.add(&self.owner, TRIANGLES, "node.make_triangles");
        self.b.set_number(tri, "src_cols", cols as f32);
        self.b.set_number(tri, "src_rows", rows as f32);
        // The sweep writes exact normals; finite differences flip where a
        // tube is thicker than its tightest turn.
        self.b.set_number(tri, "keep_normals", 1.0);
        self.b.wire(grid, "out", tri, "in");
        (tri, "out")
    }
}

/// Emit `item`'s curve shape. `None` for any other shape.
pub(crate) fn emit(b: &mut Builder<'_>, item: &Item, shape: &Shape) -> Option<CurveMesh> {
    let curve = shape.curve()?;
    let caps = shape.capped();
    let mut e = Emit {
        b,
        owner: item.id.as_str().to_owned(),
    };
    let channels = e.curve(curve);
    // Extrude and Revolve take Helix's side view: around and up.
    let profile_y = channels.rise.unwrap_or(channels.y);
    let (sweep, xy, cols, rows) = match shape {
        Shape::Extrude { .. } => {
            let xy = e.points(channels.x, profile_y);
            let sweep = e.b.add(&e.owner, SWEEP, "node.extrude_curve");
            e.b.set_number(sweep, "steps", EXTRUDE_STEPS as f32);
            e.b.set_number(sweep, "symmetric", 1.0);
            e.b.set_number(sweep, "caps", f32::from(u8::from(caps)));
            e.b.wire(xy, "out", sweep, "outline");
            (sweep, xy, SAMPLES, capped_rows(EXTRUDE_STEPS + 1, caps))
        }
        Shape::Revolve { .. } => {
            // Offset (a row) moves the curve off the axis it spins about.
            let axis = e.b.add(&e.owner, CURVE_AXIS, "node.array_math");
            e.b.set_number(axis, "op", SCALE_OFFSET);
            e.b.set_number(axis, "scale", 1.0);
            e.b.wire(channels.x.0, channels.x.1, axis, "a");
            let xy = e.points((axis, "out"), profile_y);
            let sweep = e.b.add(&e.owner, SWEEP, "node.revolve_curve");
            e.b.set_number(sweep, "segments", REVOLVE_SEGMENTS as f32);
            e.b.wire(xy, "out", sweep, "profile");
            (sweep, xy, REVOLVE_SEGMENTS + 1, SAMPLES)
        }
        Shape::Tube { .. } => {
            let xy = e.points(channels.x, channels.y);
            let sweep = e.b.add(&e.owner, SWEEP, "node.tube_from_path");
            e.b.set_number(sweep, "sides", TUBE_SIDES as f32);
            e.b.set_number(sweep, "caps", f32::from(u8::from(caps)));
            e.b.wire(xy, "out", sweep, "path");
            if let Some(rise) = channels.rise {
                e.b.wire(rise.0, rise.1, sweep, "lift");
            }
            (sweep, xy, TUBE_SIDES + 1, capped_rows(SAMPLES, caps))
        }
        _ => return None,
    };
    let vertices = e.triangles(sweep, cols, rows);
    // The solid physics and copies read: the same curve, swept on the CPU.
    let solid = e.b.add(&e.owner, SOLID, "node.curve_solid_source");
    e.b.wire(xy, "out", solid, "curve");
    if let (Some(rise), Shape::Tube { .. }) = (channels.rise, shape) {
        e.b.wire(rise.0, rise.1, solid, "lift");
    }
    let rest = rest(item, shape, curve, cols, rows);
    Some(CurveMesh {
        solid,
        vertices,
        rest,
    })
}

/// `node.curve_solid_source`'s kind for a curve shape: Extrude 0, Revolve 1,
/// Tube 2.
fn solid_kind(shape: &Shape) -> Option<u32> {
    match shape {
        Shape::Extrude { .. } => Some(0),
        Shape::Revolve { .. } => Some(1),
        Shape::Tube { .. } => Some(2),
        _ => None,
    }
}

/// Grid rows with Fill Caps: a centre and a rim row at each end.
fn capped_rows(rows: u32, caps: bool) -> u32 {
    rows + if caps { 4 } else { 0 }
}

/// Once the item's rows are applied: bind the second axis of a curve's size
/// to the row the first one has, and give the solid the sweep's params and
/// rows, so a live row moves the physics shape with the drawn one.
pub(crate) fn follow_rows(b: &mut Builder<'_>, item: &Item, shape: &Shape) {
    let owner = item.id.as_str();
    if let (Some(sweep), Some(solid)) = (b.id(owner, SWEEP), b.id(owner, SOLID)) {
        b.mirror(sweep, solid);
        if let Some(kind) = solid_kind(shape) {
            b.set(solid, "kind", SerializedParamValue::Enum { value: kind });
        }
    }
    let pair = match shape.curve() {
        Some(Curve::Circle) => (2, 3),
        Some(Curve::Helix | Curve::Lissajous) => (4, 5),
        _ => return,
    };
    if let (Some(from), Some(to)) = (
        b.id(owner, CURVE_MATH[pair.0]),
        b.id(owner, CURVE_MATH[pair.1]),
    ) {
        b.follow(from, "scale", to, "scale");
    }
}

/// The rest pose at the kind's defaults: the box the curve fills, swept.
/// A row moved live changes the solid, not this box.
fn rest(item: &Item, shape: &Shape, curve: Curve, cols: u32, rows: u32) -> Option<Rest> {
    let default = |property: &str| {
        scene_schema::row_for(&item.fields, property)
            .and_then(|row| row.default)
            .unwrap_or(0.0)
    };
    // The curve's half-extent across (x) and along (y), and Helix's rise.
    let (half_x, half_y, rise) = match curve {
        Curve::Circle | Curve::Polygon | Curve::Star | Curve::Spiral | Curve::Lissajous => {
            (default("radius"), default("radius"), None)
        }
        Curve::Helix => (
            default("radius"),
            default("radius"),
            Some(default("height") * 0.5),
        ),
        Curve::Wave => (default("amplitude"), default("length") * 0.5, None),
    };
    let profile_half_y = rise.unwrap_or(half_y);
    let (lo, hi) = match shape {
        Shape::Extrude { .. } => {
            let half_depth = default("depth") * 0.5;
            (
                [-half_x, -profile_half_y, -half_depth],
                [half_x, profile_half_y, half_depth],
            )
        }
        Shape::Revolve { .. } => {
            let reach = default("offset").abs() + half_x;
            (
                [-reach, -profile_half_y, -reach],
                [reach, profile_half_y, reach],
            )
        }
        Shape::Tube { .. } => {
            let thick = default("thickness");
            let up = rise.unwrap_or(0.0) + thick;
            (
                [-half_x - thick, -up, -half_y - thick],
                [half_x + thick, up, half_y + thick],
            )
        }
        _ => return None,
    };
    Some(Rest {
        lo,
        hi,
        placed: None,
        own_space: false,
        vertices: (cols - 1) * (rows - 1) * 6,
    })
}
