/// Clear `target` to `rgba` and commit the encoder before returning,
/// so the GPU has actually performed the clear by the time the caller
/// uses the texture.
///
/// Test-only convenience: a freshly-created encoder with a
/// `clear_texture` call recorded but never committed silently
/// discards the clear (Metal commands don't execute until commit).
/// Subsequent reads of the texture then see uninitialised
/// (often all-zero) memory, which can pass tests for the wrong
/// reason — black inputs pass against `expected = 0`, white inputs
/// fail noisily. This helper owns the encoder + commit so the bug
/// can't recur.
///
/// Stalls the calling thread until the clear completes; meant for
/// test setup, not hot-path work.
pub fn clear_texture_committed(
    device: &manifold_gpu::GpuDevice,
    target: &manifold_gpu::GpuTexture,
    rgba: [f64; 4],
    label: &str,
) {
    let mut enc = device.create_encoder(label);
    {
        let mut gpu = crate::gpu::gpu_encoder::GpuEncoder::new(&mut enc, device);
        gpu.clear_texture(target, rgba[0], rgba[1], rgba[2], rgba[3]);
    }
    enc.commit_and_wait().expect("GPU work completes");
}

// The readback helpers tests reach through the testkit are the headless
// tooling's own: one implementation of the readback and its tonemaps.
pub use crate::gpu::headless_readback::{
    encode_rgba8_png, linear_to_srgb8, mean_abs_half_diff, readback_raw_halves,
    readback_srgb_rgba8, readback_to_srgb_png_linear,
};
