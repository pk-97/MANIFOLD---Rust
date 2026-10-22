//! The physics box spawner lives on the copies body. A `node.rigid_body`
//! wired into a `node.physics_world`'s copy-group input `copies_k` is the
//! prototype that group copies, and it carries the copy count, spacing,
//! columns and arrangement. Those controls show only on a copies body, so a
//! scene with no boxes has no box sliders, and any scene gets boxes by
//! wiring a body into a copy-group input.

pub const PHYSICS_WORLD_TYPE_ID: &str = "node.physics_world";
pub const RIGID_BODY_TYPE_ID: &str = "node.rigid_body";

/// The copy group whose body input `port` is: `copies_k` is group `k`.
pub fn copy_group_of_input(port: &str) -> Option<usize> {
    match crate::physics_members::member_input(port) {
        Some((crate::physics_members::Family::Copies, k)) => Some(k as usize),
        _ => None,
    }
}
/// The spawner's params on a copies body. Each is also a scalar input of the
/// same name.
pub const COPY_PARAMS: [&str; 7] = [
    "copy_count",
    "copy_size_spacing",
    "copy_columns",
    "copy_layout",
    "copy_turn_x",
    "copy_turn_y",
    "copy_turn_z",
];
/// A copies body's Spacing default and range, in multiples of the copy's
/// size.
pub const SIZE_SPACING_DEFAULT: f32 = 1.1;
pub const SIZE_SPACING_RANGE: (f32, f32) = (1.0, 20.0);
/// The Layout labels, by stored value. Spiral and Random are
/// `node.arrange_copies`' arrangements, spaced in copy widths.
pub const COPY_LAYOUT_LABELS: [&str; 6] = ["Grid", "Scatter", "Line", "Ring", "Spiral", "Random"];
pub const COPY_LAYOUT_SCATTER: u32 = 1;

/// The most copies one world makes across all its groups. A memory ceiling, not
/// a frame budget: every copy group's instance buffer is allocated at this size
/// up front, 2 MiB on the GPU and 2 MiB on the CPU. The copy field recipes
/// (`CopyJitter`, `CopyNoise`) seed one noise sample per copy from a 256 by 256
/// grid, and `CopyBlend` lays out this many: they follow it by hand.
pub const MAX_COPIES: usize = 65_536;

/// Scatter jitters each copy by up to this fraction of its spacing per axis.
pub const SCATTER_JITTER: f32 = 0.04;

/// Half the box width of each Platonic solid at radius one, by shape index.
/// The world measures the same widths from the hulls it installs
/// (`manifold_node_engine::platonic::platonic_points`); a rigid test holds
/// the two equal bit for bit.
pub const PLATONIC_HALF_WIDTH: [f32; 5] = [0.577_350_26, 0.577_350_26, 1.0, 0.850_650_8, 0.934_172_33];

/// A Platonic copy's box width per axis at uniform `scale`, metres: what the
/// world measures from the scaled hull it installs.
pub fn platonic_width(shape: u32, scale: f32) -> [f32; 3] {
    let half = PLATONIC_HALF_WIDTH[(shape as usize).min(4)] * scale;
    [half + half; 3]
}

/// The metres one Spacing step is along each layout axis for a copy whose
/// box is `width` metres: its width along that axis, or for Scatter, whose
/// copies turn, the box diagonal widened so jittered neighbours still clear.
/// One formula for every caller.
pub fn spacing_unit(scatter: bool, width: [f32; 3]) -> [f32; 3] {
    if scatter {
        let diagonal = (width[0] * width[0] + width[1] * width[1] + width[2] * width[2]).sqrt();
        [diagonal / (1.0 - 2.0 * SCATTER_JITTER); 3]
    } else {
        width
    }
}

/// The Spacing multiple whose world spacing `multiple * unit` is `metres`
/// exactly when one exists, else the nearest.
pub fn size_multiple(metres: f32, unit: f32) -> f32 {
    let guess = metres / unit;
    let mut best = guess;
    let mut candidate_up = guess;
    let mut candidate_down = guess;
    for _ in 0..=8 {
        for candidate in [candidate_up, candidate_down] {
            if candidate * unit == metres {
                return candidate;
            }
            if (candidate * unit - metres).abs() < (best * unit - metres).abs() {
                best = candidate;
            }
        }
        candidate_up = candidate_up.next_up();
        candidate_down = candidate_down.next_down();
    }
    best
}

/// Whether `param` of `type_id` is a spawner control, stamped only on a
/// copies body.
pub fn is_copies_body_param(type_id: &str, param: &str) -> bool {
    type_id == RIGID_BODY_TYPE_ID && COPY_PARAMS.contains(&param)
}
