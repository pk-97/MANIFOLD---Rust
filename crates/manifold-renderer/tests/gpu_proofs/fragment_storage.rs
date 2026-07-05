//! Fragment-stage storage-buffer proof (RENDER_SCENE_UNBOUNDED_LIGHTS_DESIGN
//! D7). No shipped fragment entry point reads a `var<storage>` buffer through
//! the SPIRV-Cross → MSL render path — vertex stages do (render_scene's
//! `verts`), compute kernels do, fragments never have. The lights design
//! moves light data into exactly such a binding, so this proves the mechanic
//! in isolation: uniform at @binding(0) and a runtime-sized storage array at
//! @binding(8), both backed by `GpuBinding::Bytes` (setBytes on both stages),
//! read per-pixel from the fragment shader. Byte-level value assert via f16
//! readback.

use crate::harness;
use manifold_gpu::{GpuBinding, GpuTextureFormat};
use manifold_renderer::render_target::RenderTarget;

const WGSL: &str = r#"
struct Uniforms { bias: vec4<f32> }
@group(0) @binding(0) var<uniform> u: Uniforms;
@group(0) @binding(8) var<storage, read> vals: array<vec4<f32>>;

@vertex
fn vs_main(@builtin(vertex_index) vi: u32) -> @builtin(position) vec4<f32> {
    let x = f32(i32(vi & 1u) * 4 - 1);
    let y = f32(i32(vi >> 1u) * 4 - 1);
    return vec4<f32>(x, y, 0.0, 1.0);
}

@fragment
fn fs_main(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let i = u32(pos.x);
    return vals[i] + u.bias;
}
"#;

fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0f32 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let mant = (h & 0x3ff) as f32;
    match exp {
        0 => sign * mant * 2f32.powi(-24),
        0x1f => f32::NAN,
        _ => sign * (1.0 + mant / 1024.0) * 2f32.powi(exp - 15),
    }
}

#[test]
fn fragment_reads_storage_buffer_via_bytes_binding() {
    let h = harness::shared();
    let device = &h.device;
    let (w, hgt) = (4u32, 1u32);

    let pipeline = device.create_render_pipeline(
        WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba16Float,
        None,
        "frag-storage-proof",
    );
    let target = RenderTarget::new(device, w, hgt, GpuTextureFormat::Rgba16Float, "fsp-target");

    let vals: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 1.0],
        [0.0, 1.0, 0.0, 1.0],
        [0.0, 0.0, 1.0, 1.0],
        [0.25, 0.5, 0.75, 1.0],
    ];
    let bias: [f32; 4] = [0.0; 4];

    let mut enc = device.create_encoder("fsp-draw");
    enc.draw_fullscreen(
        &pipeline,
        &target.texture,
        &[
            GpuBinding::Bytes { binding: 0, data: bytemuck::bytes_of(&bias) },
            GpuBinding::Bytes { binding: 8, data: bytemuck::cast_slice(&vals) },
        ],
        true,
        true,
        "fsp-draw",
    );
    enc.commit_and_wait_completed();

    let bytes_per_row = w * 8;
    let buf = device.create_buffer_shared(u64::from(hgt * bytes_per_row));
    let mut rb = device.create_encoder("fsp-readback");
    rb.copy_texture_to_buffer(&target.texture, &buf, w, hgt, bytes_per_row);
    rb.commit_and_wait_completed();

    let ptr = buf.mapped_ptr().expect("shared readback buffer");
    let halves: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * 4) as usize) };

    for px in 0..w as usize {
        for ch in 0..4 {
            let got = half_to_f32(halves[px * 4 + ch]);
            let want = vals[px][ch];
            assert!(
                (got - want).abs() < 1e-3,
                "pixel {px} channel {ch}: got {got}, want {want} — fragment-stage \
                 storage read is BROKEN through SPIRV-Cross (lights design D7 fallback \
                 applies: fixed uniform array, escalate to Peter)"
            );
        }
    }
}
