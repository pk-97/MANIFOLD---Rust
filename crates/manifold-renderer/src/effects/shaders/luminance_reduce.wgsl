// Luminance reduction + temporal EMA for brightness stabilization.
//
// Single workgroup of 256 threads. Each thread samples the pre-tonemap
// HDR buffer at a stratified grid position (16×16 = 256 samples).
// Workgroup-parallel reduction computes the mean scene luminance.
// Thread 0 blends with the previous frame's smoothed value (EMA)
// and writes a compensation factor for the tonemap to consume.

struct LumState {
    smoothed_lum: f32,
    compensation: f32,
};

@group(0) @binding(0) var t_source: texture_2d<f32>;
@group(0) @binding(1) var s_source: sampler;
@group(0) @binding(2) var<storage, read_write> state: LumState;

// EMA blend factor. 0.3 ≈ 5-frame effective window at 60fps (~83ms).
// Fast enough to track intentional changes, slow enough to smooth noise jitter.
const ALPHA: f32 = 0.3;

var<workgroup> shared_lum: array<f32, 256>;

@compute @workgroup_size(256)
fn cs_main(@builtin(local_invocation_id) lid: vec3<u32>) {
    let idx = lid.x;

    // Stratified 16×16 grid — each thread samples one cell center.
    let cell_x = idx % 16u;
    let cell_y = idx / 16u;
    let uv = (vec2<f32>(f32(cell_x), f32(cell_y)) + 0.5) / 16.0;

    let sample = textureSampleLevel(t_source, s_source, uv, 0.0);

    // Rec.709 luminance
    shared_lum[idx] = dot(max(sample.rgb, vec3<f32>(0.0)), vec3<f32>(0.2126, 0.7152, 0.0722));

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

        // Compensation ratio: pulls current frame toward smoothed average.
        //   current > smoothed → comp < 1 → slightly darken
        //   current < smoothed → comp > 1 → slightly brighten
        let comp = smoothed / max(mean_lum, 0.0001);

        state.smoothed_lum = smoothed;
        state.compensation = comp;
    }
}
