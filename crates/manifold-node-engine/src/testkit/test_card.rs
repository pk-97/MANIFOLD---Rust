use half::f16;
use manifold_gpu::{GpuDevice, GpuTextureDesc, GpuTextureFormat, GpuTextureDimension, GpuTextureUsage};
use crate::gpu::render_target::RenderTarget;
use crate::exec::{effect_node::NodeInstanceId, execution_plan::ResourceId};

pub fn test_card_pixel(x: u32, y: u32, w: u32, h: u32) -> [f32; 4] {
    let wm = (w.max(1) - 1).max(1) as f32;
    let hm = (h.max(1) - 1).max(1) as f32;
    let u = x as f32 / wm;
    let v = y as f32 / hm;

    let third = (w / 3).max(1);
    let rgb: [f32; 3] = if x < third {
        // Left third: the old math gradient, demoted to a region (tonal/color
        // effects).
        [u, v, (u + v) * 0.5]
    } else if x < third * 2 {
        // Middle third: six vertical hue bars (R/Y/G/C/B/M) at 80% saturation
        // over a 50% gray floor (color grading).
        let bar = (((x - third) * 6) / third).min(5);
        let hue = hue_rgb(bar as f32 / 6.0);
        [
            0.5 * 0.2 + 0.8 * hue[0],
            0.5 * 0.2 + 0.8 * hue[1],
            0.5 * 0.2 + 0.8 * hue[2],
        ]
    } else if y < h / 2 {
        // Right third, top half: 2px horizontal black/white stripes
        // (sharpen/edge/glitch).
        let on = (y / 2).is_multiple_of(2);
        [f32::from(on), f32::from(on), f32::from(on)]
    } else {
        // Right third, bottom half: 8px black/white checker (blur/sharpen).
        let on = ((x / 8) + (y / 8)).is_multiple_of(2);
        [f32::from(on), f32::from(on), f32::from(on)]
    };

    // A 100%-white circle, centered over the whole frame at 15% of frame
    // height radius — the hard edge every blur/edge effect needs.
    let dx = x as f32 - w as f32 * 0.5;
    let dy = y as f32 - h as f32 * 0.5;
    let r = h as f32 * 0.15;
    let rgb = if dx * dx + dy * dy <= r * r {
        [1.0, 1.0, 1.0]
    } else {
        rgb
    };
    [rgb[0], rgb[1], rgb[2], 1.0]
}

fn hue_rgb(h: f32) -> [f32; 3] {
    let f = |n: f32| {
        let k = (n + h * 6.0) % 6.0;
        1.0 - k.min(4.0 - k).clamp(0.0, 1.0)
    };
    [f(5.0), f(3.0), f(1.0)]
}

pub fn build_test_card_input(
    device: &GpuDevice,
    w: u32,
    h: u32,
    format: GpuTextureFormat,
) -> RenderTarget {
    let mut pixels = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            let px = test_card_pixel(x, y, w, h);
            pixels[idx] = f16::from_f32(px[0]);
            pixels[idx + 1] = f16::from_f32(px[1]);
            pixels[idx + 2] = f16::from_f32(px[2]);
            pixels[idx + 3] = f16::from_f32(px[3]);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ | GpuTextureUsage::COPY_SRC,
        label: "preset-thumb-test-card-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(pixels.as_ptr().cast::<u8>(), std::mem::size_of_val(pixels.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    RenderTarget::view_of(tex, "preset-thumb-test-card-input")
}

pub fn output_resource(plan: &crate::exec::execution_plan::ExecutionPlan, node: NodeInstanceId, port: &str) -> Option<ResourceId> {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return Some(id);
                }
            }
        }
    }
    None
}
