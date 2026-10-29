//! A liquid source's pose in the world. A Water Source inside a Group sits in
//! the group's frame (its transform's `parent`); the solvers take world poses
//! with no parent. A domain's box in the world is `Transform::world_box`.

use manifold_node_engine::scene::transform::{Transform, rotation_to_render_scene_euler};

fn column_length(column: &[f32; 4]) -> f32 {
    (column[0] * column[0] + column[1] * column[1] + column[2] * column[2]).sqrt()
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
    let rotation: [[f32; 3]; 3] = std::array::from_fn(|row| {
        std::array::from_fn(|col| {
            if scale[col] > 0.0 {
                world[col][row] / scale[col]
            } else {
                0.0
            }
        })
    });
    Transform {
        pos: [world[3][0], world[3][1], world[3][2]],
        rot_euler: rotation_to_render_scene_euler(rotation),
        scale,
        parent: Transform::default().parent,
        ..transform
    }
}
