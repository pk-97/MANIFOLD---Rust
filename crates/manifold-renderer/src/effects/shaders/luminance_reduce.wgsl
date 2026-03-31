// Luminance reduction + temporal EMA for brightness stabilization.
//
// Single workgroup of 256 threads. Each thread multi-samples the pre-tonemap
// HDR buffer (4×4 sub-grid per cell = 4096 total samples across a 64×64 grid).
// Workgroup-parallel reduction computes the mean scene luminance.
// Thread 0 blends with the previous frame's smoothed value (EMA)
// and writes a clamped compensation factor for the tonemap to consume.

struct LumState {
    smoothed_lum: f32,
    compensation: f32,
};

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@group(0) @binding(2) var<storage, read_write> state: LumState;

// EMA blend factor. 0.15 ≈ 10-frame effective window at 60fps (~167ms).
const ALPHA: f32 = 0.15;

// Maximum per-frame exposure correction (±3%).
// The real noise-induced variation is 1-3%, so this is sufficient.
// Prevents measurement noise from causing visible swings.
const MAX_CORRECTION: f32 = 0.03;

var<workgroup> shared_lum: array<f32, 256>;

@compute @workgroup_size(256)
fn cs_main(@builtin(local_invocation_id) lid: vec3<u32>) {
    let idx = lid.x;

    // 16×16 cell grid, each thread covers one cell.
    let cell_x = idx % 16u;
    let cell_y = idx / 16u;
    let cell_size = 1.0 / 16.0;
    let cell_origin = vec2<f32>(f32(cell_x), f32(cell_y)) * cell_size;

    // 4×4 sub-samples within each cell = 16 samples per thread, 4096 total.
    var acc = 0.0;
    for (var sy = 0u; sy < 4u; sy++) {
        for (var sx = 0u; sx < 4u; sx++) {
            let uv = cell_origin + (vec2<f32>(f32(sx), f32(sy)) + 0.5) * (cell_size / 4.0);
            let s = textureSampleLevel(t_source, s_source, uv, 0.0);
            acc += dot(max(s.rgb, vec3<f32>(0.0)), vec3<f32>(0.2126, 0.7152, 0.0722));
        }
    }
    shared_lum[idx] = acc / 16.0; // per-thread average

    workgroupBarrier();

    // Parallel reduction (log2(256) = 8 steps)
    for (var stride = 128u; stride > 0u; stride >>= 1u) {
        if (idx < stride) {
            shared_lum[idx] += shared_lum[idx + stride];
        }
        workgroupBarrier();
    }

    if (idx == 0u) {
        let mean_lum = shared_lum[0] / 256.0;
        let prev = state.smoothed_lum;

        // First frame (prev == 0): seed with current measurement.
        let smoothed = select(
            mix(prev, mean_lum, ALPHA),
            mean_lum,
            prev <= 0.0
        );

        // Compensation: clamped to ±MAX_CORRECTION to prevent measurement
        // noise from causing visible swings. The real noise-induced variation
        // is small, so we never need large corrections.
        let raw_comp = smoothed / max(mean_lum, 0.0001);
        let comp = clamp(raw_comp, 1.0 - MAX_CORRECTION, 1.0 + MAX_CORRECTION);

        state.smoothed_lum = smoothed;
        state.compensation = comp;
    }
}
