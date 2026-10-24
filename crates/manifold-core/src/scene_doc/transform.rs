//! Item transforms as the scene compiler composes them: a group's transform
//! is the frame its children sit in (world = parent world × own TRS, the
//! `node.transform_3d` parent chain). Moving an item to another parent keeps
//! it where it is on screen (section 7.2): its new local transform is the
//! inverse of the new parent's world times its old world, as Blender's
//! Keep Transform does.

use super::{Item, ItemFields, ItemId, SceneDoc, param_id};

/// A column-major 4x4 matrix, `m[col][row]`, the layout WGSL `mat4x4<f32>`
/// and `render_scene` use.
pub type Mat4 = [[f32; 4]; 4];

pub const IDENTITY: Mat4 = [
    [1.0, 0.0, 0.0, 0.0],
    [0.0, 1.0, 0.0, 0.0],
    [0.0, 0.0, 1.0, 0.0],
    [0.0, 0.0, 0.0, 1.0],
];

/// `a × b`.
pub fn mat4_mul(a: Mat4, b: Mat4) -> Mat4 {
    let mut out = [[0.0f32; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            out[col][row] = a[0][row] * b[col][0]
                + a[1][row] * b[col][1]
                + a[2][row] * b[col][2]
                + a[3][row] * b[col][3];
        }
    }
    out
}

/// `T * R * S` from a position, XYZ Euler radians (`R = Rz * Ry * Rx`) and a
/// scale: the same matrix as `render_scene::model_matrix`.
pub fn trs_matrix(pos: [f32; 3], rot: [f32; 3], scale: [f32; 3]) -> Mat4 {
    let (cx, sx) = (rot[0].cos(), rot[0].sin());
    let (cy, sy) = (rot[1].cos(), rot[1].sin());
    let (cz, sz) = (rot[2].cos(), rot[2].sin());
    let rx: Mat4 = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, cx, sx, 0.0],
        [0.0, -sx, cx, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let ry: Mat4 = [
        [cy, 0.0, -sy, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [sy, 0.0, cy, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let rz: Mat4 = [
        [cz, sz, 0.0, 0.0],
        [-sz, cz, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    let mut m = mat4_mul(mat4_mul(rz, ry), rx);
    for (col, s) in m.iter_mut().zip(scale) {
        for v in col.iter_mut().take(3) {
            *v *= s;
        }
    }
    m[3] = [pos[0], pos[1], pos[2], 1.0];
    m
}

/// A row-major rotation matrix `r[row][col]` → the `(rot_x, rot_y, rot_z)`
/// Euler triple [`trs_matrix`] composes back into it. Reads only `r00`,
/// `r01`, `r10`, `r11`, `r20`, `r21`, `r22`. At the gimbal-lock pole
/// (`|r20| ~= 1`) z is pinned to 0 and the whole turn folds into x; `ry`
/// is set to the pole value, since `asin` near ±1 amplifies f32 rounding.
pub fn rotation_to_euler(r: [[f32; 3]; 3]) -> [f32; 3] {
    let [[r00, r01, _], [r10, r11, _], [r20, r21, r22]] = r;
    let r20c = r20.clamp(-1.0, 1.0);
    if (1.0 - r20c.abs()) < 1e-6 {
        let ry = std::f32::consts::FRAC_PI_2.copysign(-r20c);
        let rx = if r20 < 0.0 {
            r01.atan2(r11)
        } else {
            (-r01).atan2(r11)
        };
        return [rx, ry, 0.0];
    }
    [r21.atan2(r22), (-r20c).asin(), r10.atan2(r00)]
}

/// The inverse of an affine matrix; `None` when it is singular (a zero
/// scale), where no local transform can reproduce the world one.
pub fn affine_inverse(m: Mat4) -> Option<Mat4> {
    let a = |row: usize, col: usize| m[col][row];
    let det = a(0, 0) * (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1))
        - a(0, 1) * (a(1, 0) * a(2, 2) - a(1, 2) * a(2, 0))
        + a(0, 2) * (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0));
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let inv = 1.0 / det;
    // Row-major inverse of the 3x3 part.
    let r = [
        [
            (a(1, 1) * a(2, 2) - a(1, 2) * a(2, 1)) * inv,
            (a(0, 2) * a(2, 1) - a(0, 1) * a(2, 2)) * inv,
            (a(0, 1) * a(1, 2) - a(0, 2) * a(1, 1)) * inv,
        ],
        [
            (a(1, 2) * a(2, 0) - a(1, 0) * a(2, 2)) * inv,
            (a(0, 0) * a(2, 2) - a(0, 2) * a(2, 0)) * inv,
            (a(0, 2) * a(1, 0) - a(0, 0) * a(1, 2)) * inv,
        ],
        [
            (a(1, 0) * a(2, 1) - a(1, 1) * a(2, 0)) * inv,
            (a(0, 1) * a(2, 0) - a(0, 0) * a(2, 1)) * inv,
            (a(0, 0) * a(1, 1) - a(0, 1) * a(1, 0)) * inv,
        ],
    ];
    let t = [m[3][0], m[3][1], m[3][2]];
    let mut out = IDENTITY;
    for row in 0..3 {
        for col in 0..3 {
            out[col][row] = r[row][col];
        }
        out[3][row] = -(r[row][0] * t[0] + r[row][1] * t[1] + r[row][2] * t[2]);
    }
    Some(out)
}

/// Position, XYZ Euler radians and scale of an affine matrix. A shear (a
/// turned child under a non-uniformly scaled parent) has no exact TRS; the
/// nearest one keeps each axis's length and the rotation of its frame.
pub fn decompose(m: Mat4) -> ([f32; 3], [f32; 3], [f32; 3]) {
    let column = |c: usize| [m[c][0], m[c][1], m[c][2]];
    let length = |v: [f32; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    let mut scale = [length(column(0)), length(column(1)), length(column(2))];
    let det = m[0][0] * (m[1][1] * m[2][2] - m[2][1] * m[1][2])
        - m[1][0] * (m[0][1] * m[2][2] - m[2][1] * m[0][2])
        + m[2][0] * (m[0][1] * m[1][2] - m[1][1] * m[0][2]);
    // A mirrored frame keeps its handedness in one negative scale.
    if det < 0.0 {
        scale[0] = -scale[0];
    }
    let unit = |c: usize| {
        let s = if scale[c].abs() > 1e-12 {
            scale[c]
        } else {
            1.0
        };
        let v = column(c);
        [v[0] / s, v[1] / s, v[2] / s]
    };
    let (x, y, z) = (unit(0), unit(1), unit(2));
    let r = [[x[0], y[0], z[0]], [x[1], y[1], z[1]], [x[2], y[2], z[2]]];
    ([m[3][0], m[3][1], m[3][2]], rotation_to_euler(r), scale)
}

/// The transform rows an item of this kind has, as `(pos, rot, scale)`:
/// Objects, Groups, Swarms and Water Sources move, turn and scale; a Light
/// has a position only. Cameras place themselves by their rig.
fn transform_rows(item: &Item) -> (bool, bool, bool) {
    match &item.fields {
        ItemFields::Object(_)
        | ItemFields::Group(_)
        | ItemFields::Swarm(_)
        | ItemFields::WaterSource(_) => (true, true, true),
        ItemFields::Light(_) => (true, false, false),
        _ => (false, false, false),
    }
}

const POS: [&str; 3] = ["pos_x", "pos_y", "pos_z"];
const ROT: [&str; 3] = ["rot_x", "rot_y", "rot_z"];
const SCALE: [&str; 3] = ["scale_x", "scale_y", "scale_z"];

/// `id`'s own transform at the values `values` reads (a missing value is
/// the row's rest: 0, or 1 for scale).
fn local(id: &ItemId, values: &dyn Fn(&str) -> Option<f32>) -> Mat4 {
    let read =
        |names: [&str; 3], rest: f32| names.map(|name| values(&param_id(id, name)).unwrap_or(rest));
    trs_matrix(read(POS, 0.0), read(ROT, 0.0), read(SCALE, 1.0))
}

/// The world frame of whatever sits directly under `parent`: every Group
/// from the root down to it, composed.
pub fn frame_of(
    doc: &SceneDoc,
    parent: Option<&ItemId>,
    values: &dyn Fn(&str) -> Option<f32>,
) -> Mat4 {
    let Some(parent) = parent else {
        return IDENTITY;
    };
    let mut chain = Vec::new();
    doc.walk(|item, ancestors| {
        if &item.id == parent {
            chain = ancestors
                .iter()
                .chain(std::iter::once(&item))
                .map(|above| (above.id.clone(), above.kind()))
                .collect();
        }
    });
    chain
        .into_iter()
        .filter(|(_, kind)| *kind == super::ItemKind::Group)
        .fold(IDENTITY, |world, (id, _)| {
            mat4_mul(world, local(&id, values))
        })
}

/// The param writes `(param id, value)` that keep `item` where it is on
/// screen when it moves from the frame `from` to the frame `to`. Empty for
/// an item with no transform rows, or when the new frame is singular.
pub fn keep_world(
    item: &Item,
    from: Mat4,
    to: Mat4,
    values: &dyn Fn(&str) -> Option<f32>,
) -> Vec<(String, f32)> {
    let (pos, rot, scale) = transform_rows(item);
    if !pos || from == to {
        return Vec::new();
    }
    let Some(into) = affine_inverse(to) else {
        return Vec::new();
    };
    let world = mat4_mul(from, local(&item.id, values));
    let (p, r, s) = decompose(mat4_mul(into, world));
    let mut writes = Vec::new();
    let mut push = |names: [&str; 3], value: [f32; 3]| {
        for (name, value) in names.into_iter().zip(value) {
            if value.is_finite() {
                writes.push((param_id(&item.id, name), value));
            }
        }
    };
    push(POS, p);
    if rot {
        push(ROT, r);
    }
    if scale {
        push(SCALE, s);
    }
    writes
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Mat4, b: Mat4) -> bool {
        a.iter()
            .flatten()
            .zip(b.iter().flatten())
            .all(|(x, y)| (x - y).abs() < 1e-4)
    }

    #[test]
    fn a_moved_item_keeps_its_world_transform() {
        let parent = trs_matrix([1.0, 2.0, -3.0], [0.3, -0.7, 1.1], [2.0, 2.0, 2.0]);
        let local = trs_matrix([0.5, -1.0, 0.25], [-0.2, 0.4, 0.9], [0.5, 1.5, 1.0]);
        let world = mat4_mul(parent, local);
        // Out of the group to the root: the new local is the old world.
        let (p, r, s) = decompose(mat4_mul(affine_inverse(IDENTITY).unwrap(), world));
        assert!(close(trs_matrix(p, r, s), world));
        // Into another turned, scaled group.
        let other = trs_matrix([-4.0, 0.0, 2.0], [1.2, 0.1, -0.5], [0.25, 0.25, 0.25]);
        let (p, r, s) = decompose(mat4_mul(affine_inverse(other).unwrap(), world));
        assert!(close(mat4_mul(other, trs_matrix(p, r, s)), world));
    }
}
