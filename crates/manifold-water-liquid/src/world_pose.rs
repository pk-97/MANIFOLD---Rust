//! A liquid's boxes in the world. A Water or Water Source inside a Group
//! sits in the group's frame (its transform's `parent`); the solvers take
//! world poses with no parent.

use manifold_node_engine::scene::transform::{Transform, rotation_to_render_scene_euler, transform_point};

fn column_length(column: &[f32; 4]) -> f32 {
    (column[0] * column[0] + column[1] * column[1] + column[2] * column[2]).sqrt()
}

/// A domain box in the world: its centre carried through the parent frame,
/// each world side the extent of the box as the frame turns and scales it.
/// The box stays axis-aligned, since the solvers take no rotation, so a
/// turned group moves the box and fits it around the turned sides; a quarter
/// turn swaps them.
pub fn world_domain_box(transform: Transform) -> Transform {
    let parent = transform.parent;
    Transform {
        pos: transform_point(&parent, transform.pos),
        scale: std::array::from_fn(|world| (0..3).map(|local| parent[local][world].abs() * transform.scale[local]).sum()),
        parent: Transform::default().parent,
        ..transform
    }
}

/// A source's pose in the world: the parent frame times its own, taken
/// apart again into position, rotation and scale.
pub fn world_pose(transform: Transform) -> Transform {
    if transform.parent == Transform::default().parent {
        return transform;
    }
    let world = transform.world();
    let scale: [f32; 3] = std::array::from_fn(|axis| column_length(&world[axis]));
    // Row-major rotation, each column divided by its scale.
    let rotation: [[f32; 3]; 3] =
        std::array::from_fn(|row| std::array::from_fn(|col| if scale[col] > 0.0 { world[col][row] / scale[col] } else { 0.0 }));
    Transform {
        pos: [world[3][0], world[3][1], world[3][2]],
        rot_euler: rotation_to_render_scene_euler(rotation),
        scale,
        parent: Transform::default().parent,
        ..transform
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under a group turned a quarter turn about Y with scale (2, 1, 3), a
    /// domain 4 x 1 x 1 has its local X along world Z: 8 deep and 3 wide.
    #[test]
    fn a_quarter_turned_group_swaps_the_domain_sides() {
        let (s, c) = std::f32::consts::FRAC_PI_2.sin_cos();
        // Column-major: local X maps to world -Z, local Z to world X.
        let parent = [[2.0 * c, 0.0, -2.0 * s, 0.0], [0.0, 1.0, 0.0, 0.0], [3.0 * s, 0.0, 3.0 * c, 0.0], [0.0, 0.0, 0.0, 1.0]];
        let world = world_domain_box(Transform { scale: [4.0, 1.0, 1.0], parent, ..Transform::default() });
        let expected = [3.0, 1.0, 8.0];
        assert!((0..3).all(|axis| (world.scale[axis] - expected[axis]).abs() < 1e-5), "{:?}", world.scale);
    }
}
