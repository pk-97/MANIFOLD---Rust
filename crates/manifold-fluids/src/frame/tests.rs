use super::*;
use crate::{Bounds, Config, SurfaceVertex, TimeStepOptions};
use manifold_physics::UniformField;

const DT: Seconds = Seconds(1.0 / 60.0);

fn scene(min_substeps: u32, max_substeps: u32, velocity: [f32; 3]) -> FluidWorld {
    let mut world = FluidWorld::new(Config {
        cells: [12; 3],
        cell_size: 0.15,
        surface_subdivisions: 0,
        apic: false,
    })
    .unwrap();
    world.set_gravity([0.0; 3]).unwrap();
    world
        .set_time_step_options(TimeStepOptions {
            min_substeps,
            max_substeps,
            cfl: 1,
            adaptive_obstacles: true,
        })
        .unwrap();
    world
        .add_fluid_box(
            Bounds {
                min: [0.45; 3],
                max: [1.05; 3],
            },
            velocity,
        )
        .unwrap();
    world
}

fn motion(world: &mut FluidWorld) -> ([f32; 3], [f32; 3]) {
    let (mut position, mut velocity) = ([0.0; 3], [0.0; 3]);
    let ok = unsafe {
        crate::manifold_fluids_world_marker_motion(
            world.native,
            position.as_mut_ptr(),
            velocity.as_mut_ptr(),
        )
    };
    native_result(ok, "reading frame marker motion").unwrap();
    (position, velocity)
}

fn assert_same_motion(a: &mut FluidWorld, b: &mut FluidWorld) {
    let (ap, av) = motion(a);
    let (bp, bv) = motion(b);
    for (actual, expected) in ap.into_iter().chain(av).zip(bp.into_iter().chain(bv)) {
        assert!(actual.is_finite() && expected.is_finite());
        assert!((actual - expected).abs() < 2e-6, "{actual} vs {expected}");
    }
}

fn bounds(surface: &[SurfaceVertex]) -> ([f32; 3], [f32; 3]) {
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in surface {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    (min, max)
}

#[test]
fn owner_frame_matches_native_step_and_shared_field_preparation() {
    let mut regular = scene(3, 12, [0.2, -0.1, 0.05]);
    let mut staged = scene(3, 12, [0.2, -0.1, 0.05]);
    let field = UniformField::new([0.0, 0.3, 0.1]).unwrap();
    let inputs = [FieldInput {
        field: &field,
        acceleration: 0.8,
        delta_velocity: 0.2,
    }];
    for frame_index in 0..3 {
        let expected = regular.step_with_fields(DT, &inputs).unwrap();
        let mut frame = staged.begin_frame_with_fields(DT, &inputs).unwrap();
        let mut elapsed = 0.0;
        let mut substeps = 0;
        while let Some(dt) = frame.next_substep().unwrap() {
            assert_eq!(frame.next_substep().unwrap(), Some(dt));
            frame.advance(dt).unwrap();
            elapsed += dt.0;
            substeps += 1;
        }
        assert!((elapsed - DT.0).abs() < 1e-12);
        let actual = frame.finish().unwrap();
        assert_eq!(actual.substeps, substeps);
        assert_eq!(actual.substeps, expected.substeps);
        assert_eq!(actual.particles, expected.particles);
        assert_eq!(actual.triangles, expected.triangles);
        assert_same_motion(&mut regular, &mut staged);
        let (mut a, mut b) = (Vec::new(), Vec::new());
        regular.surface(&mut a).unwrap();
        staged.surface(&mut b).unwrap();
        assert!(!a.is_empty() && !b.is_empty());
        let (amin, amax) = bounds(&a);
        let (bmin, bmax) = bounds(&b);
        for (actual, expected) in amin
            .into_iter()
            .chain(amax)
            .zip(bmin.into_iter().chain(bmax))
        {
            assert!(
                (actual - expected).abs() < 2e-6,
                "frame={frame_index} bounds {actual} vs {expected}; regular={amin:?}/{amax:?}, staged={bmin:?}/{bmax:?}"
            );
        }
    }
}

#[test]
fn owner_frame_accepts_a_smaller_shared_step_without_changing_integration() {
    let mut regular = scene(2, 8, [0.2, 0.0, 0.0]);
    let mut staged = scene(1, 8, [0.2, 0.0, 0.0]);
    regular.step(DT).unwrap();
    let mut frame = staged.begin_frame(DT).unwrap();
    assert_eq!(frame.next_substep().unwrap(), Some(DT));
    frame.advance(Seconds(DT.0 / 2.0)).unwrap();
    let rest = frame.next_substep().unwrap().unwrap();
    assert!((rest.0 - DT.0 / 2.0).abs() < 1e-12);
    frame.advance(rest).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    let stats = frame.finish().unwrap();
    assert_eq!(stats.substeps, 2);
    assert_same_motion(&mut regular, &mut staged);
}

#[test]
fn owner_frame_rejects_invalid_steps_and_hides_unfinished_snapshots() {
    let mut world = scene(1, 8, [0.0; 3]);
    assert!(world.begin_frame(Seconds(f64::NAN)).is_err());
    let native = world.native;
    let mut frame = world.begin_frame(DT).unwrap();
    assert!(
        frame.advance(DT).is_err(),
        "advance requires a native offer"
    );
    let offered = frame.next_substep().unwrap().unwrap();
    assert!(frame.advance(Seconds(offered.0 * 2.0)).is_err());
    assert!(frame.advance(Seconds(0.0)).is_err());
    assert!(frame.advance(Seconds(f64::INFINITY)).is_err());
    assert_eq!(frame.next_substep().unwrap(), Some(offered));
    let (mut surface, mut len) = (std::ptr::null(), 0);
    let status = unsafe { crate::manifold_fluids_world_surface(native, &mut surface, &mut len) };
    assert!(native_result(status, "reading unfinished surface").is_err());
    let mut stats = NativeFrameStats::default();
    let status = unsafe { manifold_fluids_world_finish_frame(native, &mut stats) };
    assert!(native_result(status, "finishing prematurely").is_err());
    assert_eq!(frame.next_substep().unwrap(), Some(offered));
    frame.advance(offered).unwrap();
    assert!(frame.advance(offered).is_err());
    frame.finish().unwrap();
    world.step(DT).unwrap();
}

#[test]
fn owner_frame_abandonment_requires_rebuild() {
    let mut world = scene(2, 8, [0.0; 3]);
    {
        let mut frame = world.begin_frame(DT).unwrap();
        let dt = frame.next_substep().unwrap().unwrap();
        frame.advance(dt).unwrap();
    }
    assert!(world.step(DT).is_err());
    assert!(world.begin_frame(DT).is_err());
    assert!(world.surface(&mut Vec::new()).is_err());
    assert!(world.whitewater(&mut Vec::new()).is_err());
    let mut rebuilt = scene(1, 8, [0.0; 3]);
    rebuilt.step(DT).unwrap();
}

#[test]
fn owner_frame_exhaustion_does_not_force_a_step_beyond_the_stability_bound() {
    let mut world = scene(1, 1, [20.0, 0.0, 0.0]);
    let mut frame = world.begin_frame(DT).unwrap();
    let dt = frame.next_substep().unwrap().unwrap();
    assert!(
        dt.0 < DT.0 / 2.0,
        "CFL should restrict the fast liquid: {dt:?}"
    );
    frame.advance(dt).unwrap();
    assert!(
        frame.next_substep().is_err(),
        "exhaustion must not consume the remainder"
    );
    drop(frame);
    assert!(world.surface(&mut Vec::new()).is_err());
}

#[test]
fn live_owner_frame_consumes_the_remainder_at_the_substep_cap() {
    let mut world = scene(2, 2, [20.0, 0.0, 0.0]);
    let mut frame = world.begin_live_frame(DT).unwrap();
    let first = frame.next_substep().unwrap().unwrap();
    assert!(first.0 < DT.0, "CFL should restrict the first live substep");
    frame.advance(first).unwrap();

    let remainder = frame.next_substep().unwrap().unwrap();
    assert!(remainder.0 > 0.0);
    assert!((first.0 + remainder.0 - DT.0).abs() < 1e-12);
    frame.advance(remainder).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    let stats = frame.finish().unwrap();
    assert_eq!(stats.substeps, 2);
    assert!(stats.cap_hit);
}

#[test]
fn live_owner_frame_event_split_retains_the_numerical_offer() {
    let mut world = scene(1, 2, [20.0, 0.0, 0.0]);
    let clear = UniformField::new([0.0; 3]).unwrap();
    let mut frame = world.begin_live_frame(DT).unwrap();
    let offered = frame.next_substep().unwrap().unwrap();
    let first = Seconds(offered.0 * 0.5);
    frame.set_fields(first, &[FieldInput {
        field: &clear, acceleration: 1.0, delta_velocity: 0.0,
    }]).unwrap();
    frame.advance(first).unwrap();
    let retained = frame.next_substep().unwrap().unwrap();
    assert!((first.0 + retained.0 - offered.0).abs() < 1e-12);
    frame.advance(retained).unwrap();
    let remainder = frame.next_substep().unwrap().unwrap();
    assert!((offered.0 + remainder.0 - DT.0).abs() < 1e-12);
    frame.advance(remainder).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    let stats = frame.finish().unwrap();
    assert_eq!(stats.substeps, 3, "two numerical offers plus one event split");
    assert!(stats.cap_hit);
}

#[test]
fn live_owner_frame_preserves_20_24_30_and_60_fps_intervals() {
    for fps in [20.0, 24.0, 30.0, 60.0] {
        let duration = Seconds(1.0 / fps);
        let mut world = scene(1, 6, [0.0; 3]);
        let mut frame = world.begin_live_frame(duration).unwrap();
        let mut elapsed = 0.0;
        while let Some(step) = frame.next_substep().unwrap() {
            elapsed += step.0;
            frame.advance(step).unwrap();
        }
        frame.finish().unwrap();
        assert!((elapsed - duration.0).abs() < 1e-12, "fps={fps}");
    }
}

#[test]
fn owner_frame_short_interval_is_not_silently_lengthened() {
    let mut world = scene(1, 8, [0.0; 3]);
    let requested = Seconds(2e-7);
    let mut frame = world.begin_frame(requested).unwrap();
    assert_eq!(frame.next_substep().unwrap(), Some(requested));
    frame.advance(requested).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    frame.finish().unwrap();
}

#[test]
fn owner_frame_does_not_drop_a_small_shared_remainder() {
    let mut world = scene(1, 8, [0.0; 3]);
    let requested = Seconds(2e-7);
    let mut frame = world.begin_frame(requested).unwrap();
    let offered = frame.next_substep().unwrap().unwrap();
    let first = Seconds(offered.0 - 5e-10);
    frame.advance(first).unwrap();
    let remainder = frame.next_substep().unwrap().unwrap();
    assert_eq!(remainder.0, requested.0 - first.0);
    frame.advance(remainder).unwrap();
    assert_eq!(frame.next_substep().unwrap(), None);
    assert_eq!(frame.finish().unwrap().substeps, 2);
}

#[test]
fn owner_frame_surface_matches_completed_particle_motion() {
    let mut world = scene(4, 16, [6.0, 0.0, 0.0]);
    let mut frame = world.begin_frame(Seconds(1.0 / 30.0)).unwrap();
    while let Some(dt) = frame.next_substep().unwrap() {
        frame.advance(dt).unwrap();
    }
    let stats = frame.finish().unwrap();
    assert!(stats.substeps >= 4);
    let (particle_center, _) = motion(&mut world);
    let mut surface = Vec::new();
    world.surface(&mut surface).unwrap();
    assert!(!surface.is_empty());
    let (min, max) = bounds(&surface);
    let surface_center = (min[0] + max[0]) * 0.5;
    println!(
        "particle_x={} surface_x={surface_center}",
        particle_center[0]
    );
    // The native emitter samples this cube on a lattice, so its initial mean
    // is not exactly the authored centre. Translation along X must nevertheless
    // separate the X and Y means of the initially symmetric liquid.
    assert!(
        particle_center[0] - particle_center[1] > 0.08,
        "liquid must have moved after emission: {particle_center:?}"
    );
    // Mesh smoothing/particle sampling can move its bounds slightly. A 0.3-cell
    // tolerance rejects the observed first-substep mismatch of 0.0901 m.
    assert!((surface_center - particle_center[0]).abs() < 0.045);
}
