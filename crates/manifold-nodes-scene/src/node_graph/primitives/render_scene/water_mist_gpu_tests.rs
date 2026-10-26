//! GPU proofs for the mist's optical depth: the per-pixel `(tau, m)` against
//! the chord of the camera ray through the lattice, in closed form, for a
//! constant, a linear and a water-stopped field.
use super::mat4_inverse;
use super::water_march::{FieldDraw, FieldView, WaterMarch};
use super::water_march_gpu_tests::{
    CENTER, H, NODES, SIZE, W, depth_target, model, mul_point, pixel_ray, scene, shared_buffer,
};

/// A pixel's optical depth and first moment as the shader wrote them.
struct Mist {
    tau: f32,
    moment: f32,
}

/// Runs the mist over the test lattice and returns every pixel's result.
/// With `march_water` the level set is marched first, so the mist stops at
/// its surface.
fn run(levelset: &[f32], mist: &[f32], march_water: bool) -> Vec<Mist> {
    let device = manifold_gpu::testkit::test_device();
    let sc = scene();
    let mut enc = device.create_encoder("water mist test");
    let opaque = depth_target(&device, &mut enc);
    let view = FieldView {
        view_proj: sc.view_proj,
        prev_view_proj: sc.view_proj,
        jitter_ndc: [0.0; 2],
        prev_jitter_ndc: [0.0; 2],
        frame_seconds: 1.0 / 60.0,
        width: W,
        height: H,
    };
    let levelset = shared_buffer(&device, levelset);
    let mist = shared_buffer(&device, mist);
    let draw = FieldDraw {
        levelset: &levelset,
        velocity: None,
        mist: Some(&mist),
        center: CENTER,
        size: SIZE,
        nodes: NODES,
        model: model(),
        prev_model: model(),
        slot: 0,
    };
    let mut march = WaterMarch::default();
    march.begin(&device, &mut enc, W, H);
    if march_water {
        march
            .encode(&mut enc, &view, &draw, &opaque, None, None)
            .expect("march encodes");
    }
    let ran = march
        .encode_mist(&device, &mut enc, &view, &draw, &opaque)
        .expect("mist encodes");
    assert!(ran, "a field with a mist lattice must run the mist");
    let bytes = u64::from(W * H) * 8;
    let read = device.create_buffer_shared(bytes);
    enc.copy_buffer_to_buffer(march.mist().expect("the mist ran"), &read, bytes);
    enc.commit_and_wait_completed();
    let values: &[f32] = unsafe {
        std::slice::from_raw_parts(
            read.mapped_ptr().expect("shared").cast::<f32>(),
            (W * H * 2) as usize,
        )
    };
    values
        .chunks_exact(2)
        .map(|c| Mist {
            tau: c[0],
            moment: c[1],
        })
        .collect()
}

/// The camera ray's pass through the lattice box.
struct Chord {
    /// World distances from the ray origin to where the ray enters and leaves.
    enter: f64,
    leave: f64,
    /// Where it enters and leaves, in metres from the lattice's min corner.
    enter_local: [f64; 3],
    leave_local: [f64; 3],
}

/// The slab test of the ray against the box, on the CPU and in f64.
fn chord(origin: [f32; 3], dir: [f32; 3]) -> Option<Chord> {
    let inverse = mat4_inverse(model()).expect("the model inverts");
    let min = [
        CENTER[0] - SIZE[0] * 0.5,
        CENTER[1] - SIZE[1] * 0.5,
        CENTER[2] - SIZE[2] * 0.5,
    ];
    let o_world = mul_point(inverse, origin, 1.0);
    let o: [f64; 3] = std::array::from_fn(|i| f64::from(o_world[i] - min[i]));
    let d_raw = mul_point(inverse, dir, 0.0);
    let d_raw: [f64; 3] = std::array::from_fn(|i| f64::from(d_raw[i]));
    // World lengths times `scale` are local lengths.
    let scale = (d_raw[0] * d_raw[0] + d_raw[1] * d_raw[1] + d_raw[2] * d_raw[2]).sqrt();
    let d: [f64; 3] = std::array::from_fn(|i| d_raw[i] / scale);
    let (mut t0, mut t1) = (0.0f64, f64::MAX);
    for axis in 0..3 {
        let ta = -o[axis] / d[axis];
        let tb = (f64::from(SIZE[axis]) - o[axis]) / d[axis];
        t0 = t0.max(ta.min(tb));
        t1 = t1.min(ta.max(tb));
    }
    (t0 < t1).then(|| Chord {
        enter: t0 / scale,
        leave: t1 / scale,
        enter_local: std::array::from_fn(|i| o[i] + d[i] * t0),
        leave_local: std::array::from_fn(|i| o[i] + d[i] * t1),
    })
}

/// Six pixels spread over the image whose rays satisfy `keep`, with their chords.
fn pixels(keep: impl Fn(&Chord, [f32; 3], [f32; 3]) -> bool) -> Vec<(u32, u32, Chord)> {
    let sc = scene();
    let mut found = Vec::new();
    for y in (0..H).step_by(3) {
        for x in (0..W).step_by(5) {
            let (o, d) = pixel_ray(sc.inv, x, y);
            if let Some(c) = chord(o, d)
                && keep(&c, o, d)
            {
                found.push((x, y, c));
            }
        }
    }
    assert!(found.len() >= 6, "only {} usable pixels", found.len());
    let stride = found.len() / 6;
    found.into_iter().step_by(stride).take(6).collect()
}

fn lattice_nodes() -> usize {
    (NODES[0] * NODES[1] * NODES[2]) as usize
}

/// Node values from `f(ix, iy, iz)`, x fastest.
fn per_node(f: impl Fn(u32, u32, u32) -> f32) -> Vec<f32> {
    let mut values = Vec::with_capacity(lattice_nodes());
    for iz in 0..NODES[2] {
        for iy in 0..NODES[1] {
            for ix in 0..NODES[0] {
                values.push(f(ix, iy, iz));
            }
        }
    }
    values
}

fn assert_close(what: &str, got: f64, want: f64, relative: f64, absolute: f64) {
    assert!(
        (got - want).abs() <= relative * want.abs() + absolute,
        "{what}: {got} against {want}"
    );
}

/// Constant extinction: the optical depth is sigma times the chord, and the
/// mean position is the chord's midpoint.
#[test]
fn constant_mist_has_the_chord_for_its_depth() {
    let result = run(
        &vec![1.0; lattice_nodes()],
        &vec![0.8; lattice_nodes()],
        false,
    );
    for (x, y, c) in pixels(|c, _, _| c.leave - c.enter > 0.3) {
        let got = &result[(y * W + x) as usize];
        let length = c.leave - c.enter;
        let want = 0.8 * length;
        assert_close(
            &format!("tau at ({x},{y})"),
            f64::from(got.tau),
            want,
            1e-4,
            0.0,
        );
        assert_close(
            &format!("mean position at ({x},{y})"),
            f64::from(got.moment / got.tau),
            0.5 * (c.enter + c.leave),
            0.0,
            1e-3,
        );
    }
}

/// Extinction linear in local x: trilinear sampling and the trapezoid are both
/// exact on it, so the depth is the chord times the mean of the two end values.
#[test]
fn linear_mist_has_the_mean_of_its_end_values() {
    let sigma = |x: f64| 0.3 + 0.5 * x / f64::from(SIZE[0]);
    let mist = per_node(|ix, _, _| {
        sigma(f64::from(ix) * f64::from(SIZE[0]) / f64::from(NODES[0] - 1)) as f32
    });
    let result = run(&vec![1.0; lattice_nodes()], &mist, false);
    for (x, y, c) in pixels(|c, _, _| c.leave - c.enter > 0.3) {
        let got = &result[(y * W + x) as usize];
        let want = (c.leave - c.enter) * (sigma(c.enter_local[0]) + sigma(c.leave_local[0])) / 2.0;
        assert_close(
            &format!("tau at ({x},{y})"),
            f64::from(got.tau),
            want,
            1e-4,
            0.0,
        );
    }
}

/// A flat water surface: the mist stops at it, so the depth is sigma times the
/// air above the plane, not the whole chord.
#[test]
fn mist_stops_at_the_water() {
    // The plane sits 0.7 m above the lattice's floor, in local y.
    let plane_local = 0.7f32;
    let cell = super::water_march_gpu_tests::cell();
    let levelset = per_node(|_, iy, _| {
        let y = iy as f32 * SIZE[1] / (NODES[1] - 1) as f32;
        (y - plane_local).clamp(-4.0 * cell, 4.0 * cell)
    });
    let floor_y = CENTER[1] - SIZE[1] * 0.5;
    let plane_world = f64::from(mul_point(model(), [0.0, floor_y + plane_local, 0.0], 1.0)[1]);
    let result = run(&levelset, &vec![0.8; lattice_nodes()], true);
    let sc = scene();
    // Rays that go down, enter above the plane and reach it inside the box.
    let crossing = |c: &Chord, o: [f32; 3], d: [f32; 3]| {
        let slope = f64::from(d[1]);
        let reach = (plane_world - f64::from(o[1])) / slope;
        slope < -0.02 && reach > c.enter + 0.1 && reach < c.leave - 0.1
    };
    for (x, y, c) in pixels(crossing) {
        let got = &result[(y * W + x) as usize];
        let (o, d) = pixel_ray(sc.inv, x, y);
        let reach = (plane_world - f64::from(o[1])) / f64::from(d[1]);
        let air = reach - c.enter;
        // The hit is the bisected zero of the field: within a few 1e-4 m of the plane.
        assert_close(
            &format!("tau at ({x},{y})"),
            f64::from(got.tau),
            0.8 * air,
            1e-4,
            0.8 * 5e-4,
        );
    }
}
