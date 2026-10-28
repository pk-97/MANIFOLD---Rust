//! GPU proofs for the mist's optical depth: the per-pixel `(tau, m)` against
//! the chord of the camera ray through the field's node box, in closed form,
//! for a constant, a linear and a water-stopped field over world tiles that
//! straddle the origin.
use super::mat4_inverse;
use super::water_march::{FieldView, WaterMarch};
use super::water_march_gpu_tests::{
    CELL, FieldBuffers, H, W, depth_target, model, mul_point, pixel_ray, scene,
};
use crate::testkit::field_tiles::FieldTable;
use manifold_gpu::BufferUse;
use manifold_node_engine::scene::scene_object::FIELD_FAR_CELLS;

/// A pixel's optical depth and first moment as the shader wrote them.
struct Mist {
    tau: f32,
    moment: f32,
}

/// 4 x 2 x 4 tiles across the origin: tiles -2..1 on x and z, -1..0 on y. All
/// of their nodes are held, so inside the node box every value is exact.
fn table() -> FieldTable {
    FieldTable::new(
        CELL,
        32,
        &FieldTable::tiles_between([-2, -1, -2], [1, 0, 1]),
    )
}

/// The node box of `table()` in the object's space: the first and last held
/// node of each axis.
fn node_box(table: &FieldTable) -> ([f32; 3], [f32; 3]) {
    let e = table.extent();
    ([e[0], e[1], e[2]], [e[4], e[5], e[6]])
}

/// The extent the proofs give the shader: the node box shrunk by a cell on
/// every side. The shader widens its extent by the cell that interpolates the
/// outermost nodes, so its span is exactly the node box, where the field is
/// exact.
fn extent_inside_nodes(table: &FieldTable) -> [f32; 8] {
    let (lo, hi) = node_box(table);
    [
        lo[0] + CELL,
        lo[1] + CELL,
        lo[2] + CELL,
        0.0,
        hi[0] - CELL,
        hi[1] - CELL,
        hi[2] - CELL,
        0.0,
    ]
}

/// Marches the level set over the test table, then runs the mist, which stops
/// at the surface the march found, and returns every pixel's result.
fn run(levelset: &[f32], mist: &[f32]) -> Vec<Mist> {
    let device = manifold_gpu::testkit::test_device();
    let table = table();
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
    let buffers = FieldBuffers::new(
        &device,
        &table,
        extent_inside_nodes(&table),
        levelset,
        None,
        Some(mist),
    );
    let draw = buffers.draw(0);
    let mut march = WaterMarch::default();
    march.begin(&device, &mut enc, W, H);
    march
        .encode(&device, &mut enc, &view, &draw, &opaque, None, None, None)
        .expect("march encodes");
    let ran = march
        .encode_mist(&device, &mut enc, &view, &draw, &opaque)
        .expect("mist encodes");
    assert!(ran, "a field with a mist array must run the mist");
    let bytes = u64::from(W * H) * 8;
    let read = device.create_buffer(bytes, BufferUse::Upload);
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

/// The camera ray's pass through the node box.
struct Chord {
    /// World distances from the ray origin to where the ray enters and leaves.
    enter: f64,
    leave: f64,
    /// Where it enters and leaves, in the object's space.
    enter_local: [f64; 3],
    leave_local: [f64; 3],
}

/// The slab test of the ray against the node box, on the CPU and in f64.
fn chord(origin: [f32; 3], dir: [f32; 3]) -> Option<Chord> {
    let inverse = mat4_inverse(model()).expect("the model inverts");
    let (lo, hi) = node_box(&table());
    let o_local = mul_point(inverse, origin, 1.0);
    let o: [f64; 3] = std::array::from_fn(|i| f64::from(o_local[i]));
    let d_raw = mul_point(inverse, dir, 0.0);
    let d_raw: [f64; 3] = std::array::from_fn(|i| f64::from(d_raw[i]));
    // World lengths times `scale` are local lengths.
    let scale = (d_raw[0] * d_raw[0] + d_raw[1] * d_raw[1] + d_raw[2] * d_raw[2]).sqrt();
    let d: [f64; 3] = std::array::from_fn(|i| d_raw[i] / scale);
    let (mut t0, mut t1) = (0.0f64, f64::MAX);
    for axis in 0..3 {
        let ta = (f64::from(lo[axis]) - o[axis]) / d[axis];
        let tb = (f64::from(hi[axis]) - o[axis]) / d[axis];
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

/// Node values from `f(position)` of every held node, canonical `canon`
/// elsewhere.
fn per_node(canon: f32, f: impl Fn([f32; 3]) -> f32) -> Vec<f32> {
    let table = table();
    table.pool(canon, |p| f(table.position(p)))
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
    let far = FIELD_FAR_CELLS * CELL;
    let result = run(&per_node(far, |_| 1.0), &per_node(0.0, |_| 0.8));
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
    let far = FIELD_FAR_CELLS * CELL;
    let (lo, hi) = node_box(&table());
    let width = f64::from(hi[0] - lo[0]);
    let sigma = |x: f64| 0.3 + 0.5 * (x - f64::from(lo[0])) / width;
    let mist = per_node(0.0, |p| sigma(f64::from(p[0])) as f32);
    let result = run(&per_node(far, |_| 1.0), &mist);
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
    let far = FIELD_FAR_CELLS * CELL;
    // The plane sits at local y = -0.3, inside the node box and low enough
    // that the rays that reach it there cover a band of rows.
    let plane_local = -0.3f32;
    let levelset = per_node(far, |p| (p[1] - plane_local).clamp(-far, far));
    let plane_world = f64::from(mul_point(model(), [0.0, plane_local, 0.0], 1.0)[1]);
    let result = run(&levelset, &per_node(0.0, |_| 0.8));
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
        // The hit is the refined zero of the field: within a few 1e-4 m of the plane.
        assert_close(
            &format!("tau at ({x},{y})"),
            f64::from(got.tau),
            0.8 * air,
            1e-4,
            0.8 * 5e-4,
        );
    }
}
