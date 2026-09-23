//! `node.render_glyph_grid` — render terminal cells as grayscale glyph
//! coverage from the fixed `node.glyph_atlas` source.

use std::borrow::Cow;

use manifold_gpu::{GpuBinding, GpuSamplerDesc};

use super::standalone_pipeline::standalone_pipeline;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GlyphGridUniforms {
    columns: f32,
    rows: f32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: RenderGlyphGrid,
    type_id: "node.render_glyph_grid",
    purpose: "Render row-major Channels[VALUE: U32] terminal cells as antialiased grayscale glyph coverage using a Gather atlas. Cell values 32..126 select printable ASCII, 127 selects the inset cursor, and invalid or out-of-range cells render blank.",
    inputs: {
        atlas: Texture2D required,
        cells: Channels[VALUE: U32] required,
        columns: ScalarF32 optional,
        rows: ScalarF32 optional,
    },
    outputs: {
        out: Texture2D,
    },
    params: [
        ParamDef {
            name: Cow::Borrowed("columns"),
            label: "Columns",
            ty: ParamType::Float,
            default: ParamValue::Float(160.0),
            range: Some((1.0, 4096.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("rows"),
            label: "Rows",
            ty: ParamType::Float,
            default: ParamValue::Float(60.0),
            range: Some((1.0, 4096.0)),
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "Wire node.glyph_atlas.out into atlas and node.terminal_stream.cells into cells. columns and rows are port-shadowed scalar params; the output remains at full canvas resolution. This node emits only glyph coverage RGB with alpha 1, leaving palette and shading to later graph nodes.",
    examples: [],
    picker: { label: "Render Glyph Grid", category: Atom },
    summary: "Turns terminal cell codes into a full-resolution grayscale glyph mask.",
    category: Generate,
    role: Filter,
    aliases: ["glyph grid", "terminal renderer", "ascii renderer"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/render_glyph_grid_body.wgsl"),
    input_access: [Gather, BufferIndex],
}

impl Primitive for RenderGlyphGrid {
    fn output_canvas_scale(
        &self,
        port: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
    ) -> Option<(u32, u32)> {
        (port == "out").then_some((1, 1))
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let columns = ctx.scalar_or_param("columns", 160.0).round().max(1.0);
        let rows = ctx.scalar_or_param("rows", 60.0).round().max(1.0);
        let Some(atlas) = ctx.inputs.texture_2d("atlas") else {
            return;
        };
        let Some(cells) = ctx.inputs.array("cells") else {
            return;
        };
        let Some(out) = ctx.outputs.texture_2d("out") else {
            return;
        };
        if out.width == 0 || out.height == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        let sampler = self
            .sampler
            .get_or_insert_with(|| gpu.device.create_sampler(&GpuSamplerDesc::default()));
        let uniforms = GlyphGridUniforms {
            columns,
            rows,
            _pad0: 0,
            _pad1: 0,
        };

        // Generated standalone layout: uniform(0), Gather atlas texture(1),
        // sampler(2), BufferIndex cells storage buffer(3), output(4).
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: atlas,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: cells,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: out,
                },
            ],
            [out.width.div_ceil(16), out.height.div_ceil(16), 1],
            "node.render_glyph_grid",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::ports::{PortType, ScalarType};
    use crate::node_graph::primitive::PrimitiveSpec;

    #[test]
    fn declares_atlas_cells_and_canvas_output() {
        assert_eq!(RenderGlyphGrid::TYPE_ID, "node.render_glyph_grid");
        assert_eq!(RenderGlyphGrid::INPUTS.len(), 4);
        assert_eq!(RenderGlyphGrid::INPUTS[0].ty, PortType::Texture2D);
        assert!(RenderGlyphGrid::INPUTS[0].required);
        assert_eq!(RenderGlyphGrid::INPUTS[1].name, "cells");
        assert_eq!(
            RenderGlyphGrid::INPUTS[2].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(
            RenderGlyphGrid::INPUTS[3].ty,
            PortType::Scalar(ScalarType::F32)
        );
        assert_eq!(RenderGlyphGrid::OUTPUTS[0].ty, PortType::Texture2D);
        assert_eq!(RenderGlyphGrid::PARAMS[0].default, ParamValue::Float(160.0));
        assert_eq!(RenderGlyphGrid::PARAMS[1].default, ParamValue::Float(60.0));
    }

    #[test]
    fn output_is_canvas_sized() {
        let node = RenderGlyphGrid::new();
        assert_eq!(
            node.output_canvas_scale(
                "out",
                &crate::node_graph::effect_node::ParamValues::default(),
            ),
            Some((1, 1))
        );
    }

    #[test]
    fn uniforms_are_16_bytes() {
        assert_eq!(std::mem::size_of::<GlyphGridUniforms>(), 16);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::node_graph::freeze::codegen::{ENTRY, standalone_for_spec};
    use crate::render_target::RenderTarget;
    use half::f16;
    use manifold_gpu::{
        GpuBinding, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
    };

    const TEST_W: u32 = 4;
    const TEST_H: u32 = 4;
    const ATLAS_W: usize = 512;
    const ATLAS_H: usize = 288;
    const TILE_W: usize = 32;
    const TILE_H: usize = 48;
    const ATLAS_COLUMNS: usize = 16;
    const ASCII_FIRST: u32 = 32;

    fn synthetic_atlas(device: &manifold_gpu::GpuDevice) -> manifold_gpu::GpuTexture {
        let mut pixels = vec![0u8; ATLAS_W * ATLAS_H];
        let tile_w = TILE_W;
        let tile_h = TILE_H;
        let atlas_w = ATLAS_W;
        // A and B have deliberately distinct constant coverage values. The
        // test can then distinguish row/column orientation without depending
        // on CoreText or a particular glyph shape.
        for (codepoint, value) in [(b'A' as u32, 255u8), (b'B' as u32, 128u8), (127, 64u8)] {
            let slot = (codepoint - ASCII_FIRST) as usize;
            let tile_x = (slot % ATLAS_COLUMNS) * tile_w;
            let tile_y = (slot / ATLAS_COLUMNS) * tile_h;
            for y in 0..tile_h {
                let row = (tile_y + y) * atlas_w + tile_x;
                pixels[row..row + tile_w].fill(value);
            }
        }
        let atlas = device.create_texture(&GpuTextureDesc {
            width: ATLAS_W as u32,
            height: ATLAS_H as u32,
            depth: 1,
            format: GpuTextureFormat::R8Unorm,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD | GpuTextureUsage::SHADER_READ,
            label: "render-glyph-grid-test-atlas",
            mip_levels: 1,
        });
        device.upload_texture(&atlas, &pixels);
        atlas
    }

    fn readback_r(
        texture: &manifold_gpu::GpuTexture,
        device: &manifold_gpu::GpuDevice,
    ) -> Vec<f32> {
        let bytes_per_row = texture.width * 8;
        let buffer = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
        let mut encoder = device.create_encoder("render-glyph-grid-readback");
        encoder.copy_texture_to_buffer(
            texture,
            &buffer,
            texture.width,
            texture.height,
            bytes_per_row,
        );
        encoder.commit_and_wait_completed();
        let ptr = buffer.mapped_ptr().expect("shared readback");
        let pixels: &[u16] = unsafe {
            std::slice::from_raw_parts(
                ptr.cast::<u16>(),
                (texture.width * texture.height * 4) as usize,
            )
        };
        pixels
            .chunks_exact(4)
            .map(|pixel| f16::from_bits(pixel[0]).to_f32())
            .collect()
    }

    fn dispatch(
        device: &manifold_gpu::GpuDevice,
        atlas: &manifold_gpu::GpuTexture,
        cells: &[u32],
    ) -> Vec<f32> {
        let shader =
            standalone_for_spec::<RenderGlyphGrid>().expect("render glyph grid standalone codegen");
        let pipeline = device.create_compute_pipeline(&shader, ENTRY, "render-glyph-grid-test");
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        let cells_buffer = device.create_buffer_shared((cells.len() * 4) as u64);
        unsafe {
            cells_buffer.write(0, bytemuck::cast_slice(cells));
        }
        let output = RenderTarget::new(
            device,
            TEST_W,
            TEST_H,
            GpuTextureFormat::Rgba16Float,
            "render-glyph-grid-test-output",
        );
        let uniforms = GlyphGridUniforms {
            columns: 2.0,
            rows: 2.0,
            _pad0: 0,
            _pad1: 0,
        };
        let mut encoder = device.create_encoder("render-glyph-grid-test");
        encoder.dispatch_compute(
            &pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Texture {
                    binding: 1,
                    texture: atlas,
                },
                GpuBinding::Sampler {
                    binding: 2,
                    sampler: &sampler,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: &cells_buffer,
                    offset: 0,
                },
                GpuBinding::Texture {
                    binding: 4,
                    texture: &output.texture,
                },
            ],
            [TEST_W.div_ceil(16), TEST_H.div_ceil(16), 1],
            "render-glyph-grid-test",
        );
        encoder.commit_and_wait_completed();
        readback_r(&output.texture, device)
    }

    #[test]
    fn gpu_formula_maps_rows_columns_and_bounds_cells() {
        let device = crate::test_device();
        let atlas = synthetic_atlas(&device);

        // A/B prove the row-major cell orientation.  127 selects the cursor
        // slot (also nonzero in a real atlas), while 31 is invalid and must
        // be blank.  The four-cell buffer fills the 2×2 grid exactly.
        let full = dispatch(&device, &atlas, &[b'A' as u32, b'B' as u32, 127, 31]);
        for y in 0..TEST_H as usize {
            for x in 0..TEST_W as usize {
                let expected = if y < 2 {
                    if x < 2 { 1.0 } else { 128.0 / 255.0 }
                } else if x < 2 {
                    64.0 / 255.0
                } else {
                    0.0
                };
                let actual = full[y * TEST_W as usize + x];
                assert!(
                    (actual - expected).abs() < 0.03,
                    "pixel ({x},{y}) {actual} != {expected}"
                );
            }
        }

        // A short buffer must blank the missing bottom-right cell rather than
        // reading beyond the live storage allocation.
        let short = dispatch(&device, &atlas, &[b'A' as u32, b'B' as u32, 31]);
        assert!(short[2 * TEST_W as usize + 2] < 0.03);
        assert!(short[2 * TEST_W as usize + 3] < 0.03);
        assert!(short[0] > 0.97 && short[2] > 0.45 && short[2] < 0.55);
    }
}
