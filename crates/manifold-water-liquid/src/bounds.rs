//! The Bounds box a liquid domain publishes: its Wall and Open faces, the
//! kill height with Bounds off, its outputs and the domain box moved onto
//! whole world cells. GPU FLIP and Matter both use these, so the two solvers
//! place and open the same box.

use manifold_core::fluid_domain::FluidDomainLayout;
use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::scene::transform::Transform;
use manifold_node_engine::scene::world_tiles::{KEY_PERIOD_TILES, TILE};

/// The Bounds switch of `params`: on unless the toggle is off.
pub fn bounds_enabled(params: &ParamValues) -> bool {
    !matches!(params.get("bounds"), Some(ParamValue::Bool(false)))
}

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

/// Tiles of the key period kept clear: the table's rings round the water
/// and slack.
const KEY_MARGIN_TILES: u32 = 8;

/// This frame's scene box and kill height kept inside what tile keys tell
/// apart at cell size `h`: tiles a key period apart share a key, so the
/// water and its rings must span less. Along an axis where the scene is
/// longer, the box is cut to that span, as near centred on `sources` (the
/// box round the fills and inflows) as the scene allows, and the third value
/// says so: the water works there and holds none past it. A kill height
/// (Bounds off) is raised to that span below the box's top, so water falling
/// after a body is removed before its tiles could alias the ones above.
pub fn within_key_span(
    (scene, kill): ([[f32; 3]; 2], f32),
    sources: Option<[[f32; 3]; 2]>,
    h: f64,
) -> ([[f32; 3]; 2], f32, bool) {
    let span = KEY_PERIOD_TILES.map(|t| (f64::from((t - KEY_MARGIN_TILES) * TILE) * h) as f32);
    let mut kept = scene;
    let mut clipped = false;
    for axis in 0..3 {
        let [lo, hi] = [scene[0][axis], scene[1][axis]];
        if hi - lo <= span[axis] {
            continue;
        }
        let centre = sources.map_or(0.5 * (lo + hi), |box_| {
            0.5 * (box_[0][axis] + box_[1][axis])
        });
        let start = (centre - 0.5 * span[axis]).clamp(lo, hi - span[axis]);
        kept[0][axis] = start;
        kept[1][axis] = start + span[axis];
        clipped = true;
    }
    let kill = if kill == NO_KILL_HEIGHT {
        kill
    } else {
        kill.max(kept[1][1] - span[1])
    };
    (kept, kill, clipped)
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

/// The lowest Initial Fill or Inflow when an epoch began: each fill's lower
/// edge and each Inflow's box minimum y (`inflows`).
pub fn lowest_source(
    fills: impl IntoIterator<Item = Transform>,
    inflows: impl IntoIterator<Item = f32>,
) -> Option<f32> {
    fills
        .into_iter()
        .map(|fill| fill.pos[1] - 0.5 * fill.scale[1])
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

/// The six-bit closed-wall mask (bits −X +X −Y +Y −Z +Z) from a domain's
/// `closed_*` toggles; a missing toggle is closed.
pub fn closed_faces(params: &ParamValues) -> u32 {
    [
        "closed_neg_x",
        "closed_pos_x",
        "closed_neg_y",
        "closed_pos_y",
        "closed_neg_z",
        "closed_pos_z",
    ]
    .iter()
    .enumerate()
    .fold(0, |mask, (bit, name)| {
        let closed = !matches!(params.get(*name), Some(ParamValue::Bool(false)));
        mask | (u32::from(closed) << bit)
    })
}

/// Signed distance from `x` to the Wall faces of the Bounds box `lo`..`hi`
/// (`walls` bits −X +X −Y +Y −Z +Z): the nearest face's distance, positive on
/// the water side, `1e30` with no Wall face. Past a Wall face it is minus the
/// length of the per-axis overshoots, so two Walls meeting at an edge give a
/// Euclidean distance. Open faces are not solids and contribute nothing. The
/// step's `walls_distance` and `liquid_solid_distance_body.wgsl` compute the
/// same formula.
pub fn bounds_distance(x: [f32; 3], lo: [f32; 3], hi: [f32; 3], walls: u32) -> f32 {
    let mut nearest = 1.0e30_f32;
    let mut overshoot_squared = 0.0_f32;
    let mut beyond = false;
    for a in 0..3 {
        let mut axis_overshoot = 0.0_f32;
        if walls & (1u32 << (2 * a)) != 0 {
            let d = x[a] - lo[a];
            nearest = nearest.min(d);
            axis_overshoot = axis_overshoot.min(d);
        }
        if walls & (1u32 << (2 * a + 1)) != 0 {
            let d = hi[a] - x[a];
            nearest = nearest.min(d);
            axis_overshoot = axis_overshoot.min(d);
        }
        beyond |= axis_overshoot < 0.0;
        overshoot_squared += axis_overshoot * axis_overshoot;
    }
    if beyond {
        -overshoot_squared.sqrt()
    } else {
        nearest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_distance_is_the_nearest_wall_and_euclidean_past_walls() {
        // A box off the cell grid, different on every axis.
        let (lo, hi) = ([-1.0, -2.0, -3.0], [2.0, 3.0, 4.5]);
        let all = 0b11_1111;
        let open_top = all & !(1 << 3);
        // Inside: the nearest face. −X is 1 m away, the rest are further.
        assert_eq!(bounds_distance([0.0, 0.5, 0.0], lo, hi, all), 1.0);
        // Only the floor (−Y) is a Wall: 2.5 m up from it.
        assert_eq!(bounds_distance([0.0, 0.5, 0.0], lo, hi, 1 << 2), 2.5);
        // No Wall face: free space everywhere.
        assert_eq!(bounds_distance([0.0, 0.5, 0.0], lo, hi, 0), 1.0e30);
        assert_eq!(bounds_distance([40.0, -9.0, 7.0], lo, hi, 0), 1.0e30);
        // Past one Wall: minus the overshoot. Past −X by 3 m.
        assert_eq!(bounds_distance([-4.0, 0.5, 0.0], lo, hi, all), -3.0);
        // Past two Walls at an edge: minus the length of the overshoots,
        // 3 m past −X and 4 m past +Y.
        assert_eq!(bounds_distance([-4.0, 7.0, 0.0], lo, hi, all), -5.0);
        // The same point with +Y Open: only −X is a solid there.
        assert_eq!(bounds_distance([-4.0, 7.0, 0.0], lo, hi, open_top), -3.0);
        // Past an Open face alone is free space, and the Open face's own
        // distance never lowers the answer.
        assert_eq!(bounds_distance([0.0, 7.0, 0.0], lo, hi, open_top), 1.0);
        // On the plane of a Wall the distance is 0.
        assert_eq!(bounds_distance([2.0, 0.5, 0.0], lo, hi, all), 0.0);
    }

    /// At 1 mm the water spans 504 tiles on y (4.032 m) and 2040 on x and z
    /// (16.32 m). A longer scene is cut to the span round its sources, never
    /// past its own ends, and a deep kill height is lifted to the span below
    /// the kept top.
    #[test]
    fn the_key_span_cuts_a_long_scene_round_its_sources() {
        let h = 0.001;
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let tall = [[0.0, 0.0, 0.0], [1.0, 10.0, 1.0]];
        let high_tap = Some([[0.0, 6.0, 0.0], [1.0, 7.0, 1.0]]);
        let (kept, kill, clipped) = within_key_span((tall, -1.0), high_tap, h);
        assert!(clipped);
        assert!(
            near(kept[0][1], 6.5 - 2.016) && near(kept[1][1], 6.5 + 2.016),
            "{kept:?}"
        );
        assert!(near(kill, kept[0][1]), "{kill}");
        assert_eq!(kept[0][0], 0.0, "the short axes stay");
        // Sources near an end keep the span inside the scene.
        let low_tap = Some([[0.0, 0.0, 0.0], [1.0, 0.5, 1.0]]);
        let (kept, ..) = within_key_span((tall, NO_KILL_HEIGHT), low_tap, h);
        assert!(near(kept[0][1], 0.0) && near(kept[1][1], 4.032), "{kept:?}");
        let wide = [[-20.0, 0.0, 0.0], [20.0, 1.0, 1.0]];
        let (kept, ..) = within_key_span((wide, -1.0), None, h);
        assert!(
            near(kept[0][0], -8.16) && near(kept[1][0], 8.16),
            "{kept:?}"
        );
        let scene = [[0.0, 0.0, 0.0], [1.0, 2.0, 1.0]];
        let (kept, deep, clipped) = within_key_span((scene, -100.0), None, h);
        assert!(!clipped && kept == scene);
        assert!(near(deep, 2.0 - 4.032), "{deep}");
        assert_eq!(within_key_span((scene, -0.5), None, h).1, -0.5);
        assert_eq!(
            within_key_span((scene, NO_KILL_HEIGHT), None, h).1,
            NO_KILL_HEIGHT
        );
    }
}
