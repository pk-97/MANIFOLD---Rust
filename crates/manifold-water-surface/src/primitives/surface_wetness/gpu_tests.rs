//! GPU proofs for `node.surface_wetness`: contact soaks, then it dries at the
//! film's evaporation rate and compounds frame to frame, per slot of the
//! display tiles; a slot that takes another tile starts dry.

use super::*;
use manifold_node_engine::bindings::Slot;
use manifold_node_engine::exec::backend::Backend;
use manifold_node_engine::scene::world_tiles::{TILE_CELLS, pool_len};
use manifold_node_engine::testkit::array_harness::{Harness, params, read};
use manifold_water_liquid::testkit::world_box::table_words_of;

const H: f32 = 0.1;
const POOL: u32 = 2;

/// The tiles a rig's table holds: a tile and its slot each.
type Held = [([i32; 3], u32)];

/// Tiles -1 and 0 in x, either side of the origin.
const ACROSS_THE_ORIGIN: &[([i32; 3], u32)] = &[([-1, 0, 0], 0), ([0, 0, 0], 1)];

fn elements() -> usize {
    pool_len(POOL) as usize
}

/// The world cell a pool element holds; `None` for a free slot and the
/// canonical element.
fn cell_of(held: &Held, element: usize) -> Option<[i32; 3]> {
    let slot = (element / TILE_CELLS as usize) as u32;
    let (tile, _) = held.iter().find(|(_, s)| *s == slot)?;
    let local = element % TILE_CELLS as usize;
    Some([
        tile[0] * 8 + (local % 8) as i32,
        tile[1] * 8 + (local / 8 % 8) as i32,
        tile[2] * 8 + (local / 64) as i32,
    ])
}

/// A level set with water (negative) at the cells `wet` picks and three cells
/// of air, the canonical value, everywhere else.
fn field(held: &Held, wet: impl Fn([i32; 3]) -> bool) -> Vec<f32> {
    (0..elements())
        .map(|e| match cell_of(held, e) {
            Some(cell) if wet(cell) => -1.0,
            _ => 3.0 * H,
        })
        .collect()
}

/// A node and everything it needs across runs: the wired time and epoch, the
/// output slot, and the node itself.
struct Rig {
    harness: Harness,
    node: SurfaceWetness,
    time: Slot,
    epoch: Slot,
    out: Slot,
    drying: f32,
}

impl Rig {
    fn new(drying: f32) -> Self {
        let mut harness = Harness::new();
        let time = harness.scalar_input(0.0);
        let epoch = harness.scalar_input(0.0);
        let (out, _) = harness.array::<f32>(&[], 1);
        Self {
            harness,
            node: SurfaceWetness::new(),
            time,
            epoch,
            out,
            drying,
        }
    }

    /// One run with `held` in the table over `field` at `time` and `epoch`;
    /// returns the wetness.
    fn run(&mut self, held: &Held, field: &[f32], time: f32, epoch: f32) -> Vec<f32> {
        let words = table_words_of(POOL, held);
        let (tiles_slot, _) = self.harness.array(&words, words.len());
        let (field_slot, _) = self.harness.array(field, field.len());
        self.harness
            .backend
            .set_scalar(self.time, ParamValue::Float(time));
        self.harness
            .backend
            .set_scalar(self.epoch, ParamValue::Float(epoch));
        let run_params = params(&[
            ("drying", self.drying),
            ("cell_size", H),
            ("pool_tiles", POOL as f32),
        ]);
        let (_, errors) = self.harness.run(
            &mut self.node,
            &[
                ("field", field_slot),
                ("tiles", tiles_slot),
                ("time", self.time),
                ("epoch", self.epoch),
            ],
            &[("wetness", self.out)],
            &run_params,
        );
        assert!(errors.is_empty(), "{errors:?}");
        read(&self.harness.buffer(self.out), elements())
    }
}

/// Water on the low-x side of both tiles' shared face and a little past it.
fn half_wet(held: &Held) -> Vec<f32> {
    field(held, |cell| cell[0] <= 1)
}

fn dry(held: &Held) -> Vec<f32> {
    field(held, |_| false)
}

#[test]
fn contact_soaks_and_the_rest_starts_dry() {
    let mut rig = Rig::new(1.0);
    let wetness = rig.run(ACROSS_THE_ORIGIN, &half_wet(ACROSS_THE_ORIGIN), 5.0, 0.0);
    let mut soaked = 0;
    for (e, &w) in wetness.iter().enumerate() {
        let want = match cell_of(ACROSS_THE_ORIGIN, e) {
            Some(cell) if cell[0] <= 1 => 1.0,
            _ => 0.0,
        };
        assert_eq!(w, want, "element {e}");
        soaked += usize::from(w == 1.0);
    }
    // Tile -1 whole and two columns of tile 0.
    assert_eq!(soaked, 512 + 2 * 64);
}

#[test]
fn wet_nodes_dry_at_the_film_rate_and_compound() {
    let mut rig = Rig::new(10.0);
    let first = rig.run(ACROSS_THE_ORIGIN, &half_wet(ACROSS_THE_ORIGIN), 5.0, 0.0);
    let wet: Vec<usize> = (0..elements()).filter(|&e| first[e] == 1.0).collect();
    assert!(!wet.is_empty());
    for (total, now) in [(60.0_f64, 65.0), (120.0, 125.0)] {
        let wetness = rig.run(ACROSS_THE_ORIGIN, &dry(ACROSS_THE_ORIGIN), now, 0.0);
        let want = (-10.0 * total / f64::from(DRY_SECONDS)).exp() as f32;
        for (e, &w) in wetness.iter().enumerate() {
            if wet.contains(&e) {
                assert!(
                    (w - want).abs() <= 1e-5 * want,
                    "t+{total}: element {e} reads {w}, expected {want}"
                );
            } else {
                assert_eq!(w, 0.0, "t+{total}: element {e}");
            }
        }
    }
}

#[test]
fn a_slot_that_took_another_tile_or_was_free_starts_dry() {
    let mut rig = Rig::new(10.0);
    let all_wet = field(ACROSS_THE_ORIGIN, |_| true);
    let first = rig.run(ACROSS_THE_ORIGIN, &all_wet, 5.0, 0.0);
    assert!((0..2 * 512).all(|e| first[e] == 1.0));
    let after = |seconds: f64| (-10.0 * seconds / f64::from(DRY_SECONDS)).exp() as f32;
    let slot_values = |wetness: &[f32], slot: usize| wetness[slot * 512..(slot + 1) * 512].to_vec();

    // Slot 1 now holds tile 7 in x: the dry tile it was before carries
    // nothing, while slot 0 keeps its tile and its film.
    let moved: &Held = &[([-1, 0, 0], 0), ([7, 0, 0], 1)];
    let wetness = rig.run(moved, &dry(moved), 65.0, 0.0);
    assert!(
        slot_values(&wetness, 0)
            .iter()
            .all(|&w| (w - after(60.0)).abs() <= 1e-5)
    );
    assert!(slot_values(&wetness, 1).iter().all(|&w| w == 0.0));
    assert_eq!(wetness[elements() - 1], 0.0, "the canonical element");

    // Back to tile 0 in slot 1: another change of tile, another start.
    let wetness = rig.run(ACROSS_THE_ORIGIN, &dry(ACROSS_THE_ORIGIN), 125.0, 0.0);
    assert!(
        slot_values(&wetness, 0)
            .iter()
            .all(|&w| (w - after(120.0)).abs() <= 1e-5)
    );
    assert!(slot_values(&wetness, 1).iter().all(|&w| w == 0.0));

    // A free slot reads 0 and slot 0 goes on drying.
    let only_first: &Held = &[([-1, 0, 0], 0)];
    let wetness = rig.run(only_first, &dry(only_first), 185.0, 0.0);
    assert!(
        slot_values(&wetness, 0)
            .iter()
            .all(|&w| (w - after(180.0)).abs() <= 1e-5)
    );
    assert!(slot_values(&wetness, 1).iter().all(|&w| w == 0.0));
}

#[test]
fn time_running_backwards_or_a_new_epoch_starts_over() {
    // Time goes back from 100 s to 40 s.
    let mut rig = Rig::new(10.0);
    rig.run(ACROSS_THE_ORIGIN, &half_wet(ACROSS_THE_ORIGIN), 100.0, 0.0);
    let wetness = rig.run(ACROSS_THE_ORIGIN, &dry(ACROSS_THE_ORIGIN), 40.0, 0.0);
    assert!(wetness.iter().all(|&w| w == 0.0));

    // Time moves on, but the epoch changes.
    let mut rig = Rig::new(10.0);
    rig.run(ACROSS_THE_ORIGIN, &half_wet(ACROSS_THE_ORIGIN), 100.0, 1.0);
    let wetness = rig.run(ACROSS_THE_ORIGIN, &dry(ACROSS_THE_ORIGIN), 101.0, 2.0);
    assert!(wetness.iter().all(|&w| w == 0.0));
}
