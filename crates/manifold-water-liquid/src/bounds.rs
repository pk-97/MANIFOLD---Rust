//! The Bounds box a liquid domain publishes: its Wall and Open faces, the
//! kill height with Bounds off, its outputs and the domain box moved onto
//! whole world cells. GPU FLIP and Matter both use these, so the two solvers
//! place and open the same box.

use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_node_engine::scene::transform::Transform;

/// The world cell a box minimum corner starts at: the nearest whole cell.
pub fn box_cell(min: f32, cell_size: f64) -> i32 {
    (f64::from(min) / cell_size).round() as i32
}

/// `layout` with its minimum corner on a whole world cell. Cell p of the world
/// is centred on p·h, so a box that starts on one has its cells on the world
/// lattice and its fill sites at fixed world positions, whatever the domain's
/// pose. The corner moves by less than half a cell; the size and the cell
/// count stay.
pub fn on_world_cells(layout: FluidDomainLayout) -> FluidDomainLayout {
    let h = layout.cell_size;
    FluidDomainLayout {
        min: layout.min.map(|v| (f64::from(box_cell(v, h)) * h) as f32),
        ..layout
    }
}

/// The box of `size` centred on `pos` (min then max).
pub fn box_about(pos: [f32; 3], size: [f32; 3]) -> [[f32; 3]; 2] {
    [
        std::array::from_fn(|i| pos[i] - 0.5 * size[i]),
        std::array::from_fn(|i| pos[i] + 0.5 * size[i]),
    ]
}

/// All six faces, bits −X +X −Y +Y −Z +Z (the order of `closed_faces`).
pub const ALL_FACES: u32 = 0b11_1111;

/// The Wall and Open masks of a frame's faces: `closed` holds the Closed
/// toggles, a closed face being Wall and an open one Open. With Bounds off
/// there are no faces, so neither mask has a bit.
pub fn face_masks(bounds: bool, closed: u32) -> (u32, u32) {
    if bounds {
        (closed & ALL_FACES, ALL_FACES & !closed)
    } else {
        (0, 0)
    }
}

/// The kill height Bounds on publishes, below every particle: nothing reads it.
pub const NO_KILL_HEIGHT: f32 = f32::MIN;

/// The kill height (m) with Bounds off: the lowest solid point minus
/// `drawn_radius`. The lowest solid point is the lowest world-box minimum y
/// over the Fixed bodies, `bodies` being (is Fixed, box minimum y); with no
/// Fixed body, over every body; with no body, `source_floor`, the lowest
/// Initial Fill or Inflow when the epoch began. A body that falls forever
/// must not drag the plane down with it, which is why Fixed bodies come first.
/// With nothing placed there is nothing to fall past: [`NO_KILL_HEIGHT`].
pub fn kill_height(
    bodies: impl IntoIterator<Item = (bool, f32)>,
    source_floor: Option<f32>,
    drawn_radius: f32,
) -> f32 {
    let lowest = |a: Option<f32>, y: f32| Some(a.map_or(y, |a| a.min(y)));
    let (fixed, any) = bodies
        .into_iter()
        .fold((None, None), |(fixed, any), (is_fixed, y)| {
            (
                if is_fixed { lowest(fixed, y) } else { fixed },
                lowest(any, y),
            )
        });
    match fixed.or(any).or(source_floor) {
        Some(floor) => floor - drawn_radius,
        None => NO_KILL_HEIGHT,
    }
}

/// The box round `boxes`, or None when nothing is placed or the box has no
/// finite extent.
pub fn union_box(boxes: impl IntoIterator<Item = [[f32; 3]; 2]>) -> Option<[[f32; 3]; 2]> {
    let union = boxes.into_iter().reduce(|a, b| {
        [
            std::array::from_fn(|i| a[0][i].min(b[0][i])),
            std::array::from_fn(|i| a[1][i].max(b[1][i])),
        ]
    })?;
    let longest = (0..3)
        .map(|i| union[1][i] - union[0][i])
        .fold(0.0, f32::max);
    (longest.is_finite() && longest > 0.0).then_some(union)
}

/// The six Bounds outputs for the box `scene`.
pub fn bounds_outputs(scene: [[f32; 3]; 2]) -> [(&'static str, f32); 6] {
    [
        ("bounds_lo_x", scene[0][0]),
        ("bounds_lo_y", scene[0][1]),
        ("bounds_lo_z", scene[0][2]),
        ("bounds_hi_x", scene[1][0]),
        ("bounds_hi_y", scene[1][1]),
        ("bounds_hi_z", scene[1][2]),
    ]
}

/// The Bounds faces' velocities along their axes (m/s): `now` minus
/// `previous` over `seconds` of simulated time. 0 without a previous box (the
/// first frame, a restart, Bounds off) and when no time ran (a paused drag is
/// a teleport).
pub fn wall_velocity_outputs(
    previous: Option<[[f32; 3]; 2]>,
    now: [[f32; 3]; 2],
    seconds: f64,
) -> [(&'static str, f32); 6] {
    let speed = |end: usize, axis: usize| match previous {
        Some(before) if seconds > 0.0 => {
            ((f64::from(now[end][axis]) - f64::from(before[end][axis])) / seconds) as f32
        }
        _ => 0.0,
    };
    [
        ("wall_velocity_lo_x", speed(0, 0)),
        ("wall_velocity_lo_y", speed(0, 1)),
        ("wall_velocity_lo_z", speed(0, 2)),
        ("wall_velocity_hi_x", speed(1, 0)),
        ("wall_velocity_hi_y", speed(1, 1)),
        ("wall_velocity_hi_z", speed(1, 2)),
    ]
}

/// The lowest Initial Fill or Inflow when an epoch began: the fill's lower
/// edge and each Inflow's box minimum y (`inflows`).
pub fn lowest_source(
    fill: Option<Transform>,
    inflows: impl IntoIterator<Item = f32>,
) -> Option<f32> {
    fill.map(|fill| fill.pos[1] - 0.5 * fill.scale[1])
        .into_iter()
        .chain(inflows)
        .reduce(f32::min)
}

/// What an epoch started with: the kill height hangs from it while no body is
/// placed. Taken once per epoch, so a source that moves later does not move
/// the plane.
#[derive(Default)]
pub struct SourceFloor {
    epoch: Option<u32>,
    y: Option<f32>,
}

impl SourceFloor {
    /// The floor of `epoch`: `floor()` the first time it is asked for that
    /// epoch, the same value after.
    pub fn of_epoch(&mut self, epoch: u32, floor: impl FnOnce() -> Option<f32>) -> Option<f32> {
        if self.epoch != Some(epoch) {
            *self = Self {
                epoch: Some(epoch),
                y: floor(),
            };
        }
        self.y
    }
}
