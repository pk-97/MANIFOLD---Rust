//! GPU value proofs for the running total against a CPU scan. The atom runs
//! through its own `run()` on a real device with pre-bound shared buffers;
//! scalar inputs arrive as the params they shadow.

use manifold_node_engine::exec::effect_node::ParamValues;
use manifold_node_engine::parameters::ParamValue;
use manifold_water_liquid::primitives::running_total::RunningTotal;

use manifold_node_engine::testkit::array_harness::{Harness, params, read};

/// Deterministic pseudo-random stream (xorshift) for fixtures.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed)
    }
    fn next_f32(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

#[test]
fn fluid_running_total_matches_cpu_scan_and_total_lags_one_frame() {
    let mut harness = Harness::new();
    let mut rng = Rng::new(7);
    // (1 << 24) + 3 needs four scan levels, as Surface Detail 2 at resolution 64 does.
    for size in [1usize, 255, 256, 257, (1 << 20) + 3, (1 << 24) + 3] {
        let values: Vec<u32> = (0..size).map(|_| (rng.next_f32() * 6.0) as u32).collect();
        let (input, _) = harness.array(&values, size);
        let (out_slot, out) = harness.array::<u32>(&[], size);
        let total_slot = harness.scalar();
        let mut node = RunningTotal::new();
        let run = |node: &mut RunningTotal, harness: &mut Harness| {
            harness.run(
                node,
                &[("in", input)],
                &[("out", out_slot), ("total", total_slot)],
                &ParamValues::default(),
            )
        };
        let (scalars, errors) = run(&mut node, &mut harness);
        assert!(errors.is_empty(), "{errors:?}");
        let first_total = scalars
            .iter()
            .find(|(slot, _)| *slot == total_slot)
            .map(|(_, v)| v.clone());
        assert_eq!(
            first_total,
            Some(ParamValue::Float(0.0)),
            "a fresh node reports nothing yet"
        );
        let mut running = 0u64;
        let expected: Vec<u32> = values
            .iter()
            .map(|&v| {
                running += u64::from(v);
                running as u32
            })
            .collect();
        let actual: Vec<u32> = read(&out, size);
        let first_wrong = expected.iter().zip(&actual).position(|(e, a)| e != a);
        assert_eq!(first_wrong, None, "size {size}");
        let (scalars, _) = run(&mut node, &mut harness);
        let lagged = scalars
            .iter()
            .find(|(slot, _)| *slot == total_slot)
            .map(|(_, v)| v.clone());
        assert_eq!(
            lagged,
            Some(ParamValue::Float(running as f32)),
            "size {size}: one frame late"
        );
    }
}

/// A total past the consumer's capacity is a named error on every frame it
/// lasts, never silent; at or under capacity there is none.
#[test]
fn fluid_running_total_names_a_total_past_its_capacity() {
    let mut harness = Harness::new();
    let flags = vec![1u32; 100];
    let (input, _) = harness.array(&flags, flags.len());
    let (out_slot, _) = harness.array::<u32>(&[], flags.len());
    for (capacity, expect_error) in [(100.0, false), (64.0, true)] {
        let mut node = RunningTotal::new();
        let mut params = ParamValues::default();
        params.insert("capacity".into(), ParamValue::Float(capacity));
        let mut run = || {
            harness
                .run(&mut node, &[("in", input)], &[("out", out_slot)], &params)
                .1
        };
        assert!(run().is_empty(), "the first frame has no total yet");
        for frame in 1..3 {
            let errors = run();
            if expect_error {
                assert!(
                    errors.iter().any(|e| e.contains("needs 100, holds 64")),
                    "frame {frame}: an overflow must be named every frame: {errors:?}"
                );
            } else {
                assert!(
                    errors.is_empty(),
                    "frame {frame}: capacity {capacity} holds 100: {errors:?}"
                );
            }
        }
    }
}

/// `extent` holds the total, then 256-thread groups covering max(total, last
/// frame's total) × per_item elements.
#[test]
fn fluid_running_total_extent_covers_this_and_last_frame() {
    let mut harness = Harness::new();
    let mut node = RunningTotal::new();
    let (out, _) = harness.array::<u32>(&[], 1000);
    let (extent_slot, extent) = harness.array::<u32>(&[], 4);
    let total = harness.scalar();
    let per_item = params(&[("per_item", 3.0)]);
    // (live items, expected groups): 300 elements → 2 groups; shrinking to 10
    // still covers last frame's 300; then 30 elements → 1 group.
    for (items, groups) in [(100u32, 2u32), (10, 2), (10, 1)] {
        let values: Vec<u32> = (0..1000).map(|i| u32::from(i < items)).collect();
        let (input, _) = harness.array(&values, 1000);
        let (_, errors) = harness.run(
            &mut node,
            &[("in", input)],
            &[("out", out), ("total", total), ("extent", extent_slot)],
            &per_item,
        );
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(
            read::<u32>(&extent, 4),
            vec![items, groups, 1, 1],
            "{items} live items"
        );
    }
}
