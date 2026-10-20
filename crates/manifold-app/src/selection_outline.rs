//! The selection outline (D-102): a silhouette in a highlight colour around
//! the selected object, on the output preview and the graph editor's
//! viewport, never in the output picture (REALTIME_3D_DESIGN.md D9).
//!
//! The method is Blender's: the scene renderer draws the selected objects'
//! coverage into a mask through the camera the picture was rendered with
//! (`render_scene/selection_mask.rs`); the content thread copies it into a
//! mask bridge beside the picture's own bridge, on the same surface slot, so
//! the picture's read lease covers both; the UI draws the outline over the
//! picture by testing each pixel's neighbourhood in the mask at a fixed
//! on-screen width.

use std::sync::Arc;

use manifold_gpu::{GpuBinding, GpuDevice, GpuEncoder, GpuRenderPipeline, GpuSampler, GpuTexture};
pub(crate) use manifold_nodes_scene::node_graph::viewport_gizmo::outline_slots;

#[cfg(target_os = "macos")]
use crate::shared_texture::{SURFACE_COUNT, SharedTextureBridge};

/// Outline colour, straight alpha: Blender's selection orange.
const OUTLINE_COLOR: [f32; 4] = [1.0, 0.63, 0.16, 1.0];
/// Outline width in screen points.
const OUTLINE_WIDTH_POINTS: f32 = 2.0;

/// The mask bridge's content side: one surface per output slot, sized to
/// the mask the scene drew.
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(crate) struct OutlineMasks {
    bridge: Option<Arc<SharedTextureBridge>>,
    textures: [Option<GpuTexture>; SURFACE_COUNT],
}

#[cfg(target_os = "macos")]
impl OutlineMasks {
    pub fn install(&mut self, device: &GpuDevice, bridge: Arc<SharedTextureBridge>) {
        // SAFETY: `self.bridge` keeps the IOSurfaces alive as long as the textures.
        self.textures = std::array::from_fn(|i| Some(unsafe { bridge.import_texture_native(device, i) }));
        self.bridge = Some(bridge);
    }

    /// Copy this frame's `mask` into surface `slot`, resizing the bridge to
    /// it first. The bridge generation the frame records, or `None` when
    /// there is no mask.
    pub fn write(&mut self, device: &GpuDevice, enc: &mut GpuEncoder, slot: usize, mask: Option<&GpuTexture>) -> Option<u64> {
        let (mask, bridge) = (mask?, self.bridge.as_ref()?);
        if bridge.width() != mask.width || bridge.height() != mask.height {
            bridge.resize(mask.width, mask.height);
            // SAFETY: as in `install`.
            self.textures = std::array::from_fn(|i| Some(unsafe { bridge.import_texture_native(device, i) }));
        }
        enc.copy_texture_to_texture(mask, self.textures[slot].as_ref()?, mask.width, mask.height, 1);
        Some(bridge.generation())
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct OutlineUniforms {
    color: [f32; 4],
    /// Mask UV per screen pixel.
    texel: [f32; 2],
    /// Outline width in screen pixels.
    radius: f32,
    _pad: f32,
}

const OUTLINE_WGSL: &str = r#"
@group(0) @binding(0) var mask: texture_2d<f32>;
@group(0) @binding(1) var mask_sampler: sampler;
struct Outline { color: vec4<f32>, texel: vec2<f32>, radius: f32, pad: f32 };
@group(0) @binding(2) var<uniform> u: Outline;
struct VertexOutput {
    @builtin(position) position: vec4<f32>,
    @location(0) uv: vec2<f32>,
};
@vertex
fn vs_main(@builtin(vertex_index) idx: u32) -> VertexOutput {
    var out: VertexOutput;
    let x = f32(i32(idx) / 2) * 4.0 - 1.0;
    let y = f32(i32(idx) % 2) * 4.0 - 1.0;
    out.position = vec4<f32>(x, y, 0.0, 1.0);
    out.uv = vec2<f32>((x + 1.0) * 0.5, (1.0 - y) * 0.5);
    return out;
}
// Outside the silhouette, within `radius` of it: the outline. Two rings of
// sixteen taps; the mask's linear filtering softens the edge.
@fragment
fn fs_main(in: VertexOutput) -> @location(0) vec4<f32> {
    let inside = textureSampleLevel(mask, mask_sampler, in.uv, 0.0).r;
    var near = 0.0;
    for (var i = 0u; i < 16u; i = i + 1u) {
        let a = f32(i) * 0.39269908;
        let dir = vec2<f32>(cos(a), sin(a)) * u.texel;
        near = max(near, textureSampleLevel(mask, mask_sampler, in.uv + dir * u.radius, 0.0).r);
        near = max(near, textureSampleLevel(mask, mask_sampler, in.uv + dir * u.radius * 0.5, 0.0).r);
    }
    let alpha = clamp(near - inside, 0.0, 1.0) * u.color.a;
    return vec4<f32>(u.color.rgb * alpha, alpha);
}
"#;

/// The UI pass that draws the outline over a picture.
pub(crate) struct SelectionOutline {
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
}

impl SelectionOutline {
    pub fn new(device: &GpuDevice) -> Self {
        let premultiplied = manifold_gpu::GpuBlendState {
            src_factor: manifold_gpu::GpuBlendFactor::One,
            dst_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            operation: manifold_gpu::GpuBlendOp::Add,
            src_alpha_factor: manifold_gpu::GpuBlendFactor::One,
            dst_alpha_factor: manifold_gpu::GpuBlendFactor::OneMinusSrcAlpha,
            alpha_operation: manifold_gpu::GpuBlendOp::Add,
        };
        Self {
            pipeline: device.create_render_pipeline(
                OUTLINE_WGSL,
                "vs_main",
                "fs_main",
                manifold_compositor::presentation::UI_FORMAT,
                Some(premultiplied),
                "Selection Outline",
            ),
            sampler: device.create_sampler(&manifold_gpu::GpuSamplerDesc {
                min_filter: manifold_gpu::GpuFilterMode::Linear,
                mag_filter: manifold_gpu::GpuFilterMode::Linear,
                ..Default::default()
            }),
        }
    }

    /// Outline `mask` over the picture at `viewport` (physical pixels) in
    /// `target`, `scale` physical pixels per point.
    pub fn draw(&self, encoder: &mut GpuEncoder, target: &GpuTexture, mask: &GpuTexture, viewport: (f32, f32, f32, f32), scale: f32) {
        let (_, _, width, height) = viewport;
        if width < 1.0 || height < 1.0 {
            return;
        }
        let uniforms = OutlineUniforms {
            color: OUTLINE_COLOR,
            texel: [1.0 / width, 1.0 / height],
            radius: OUTLINE_WIDTH_POINTS * scale,
            _pad: 0.0,
        };
        encoder.draw_fullscreen_viewport(
            &self.pipeline,
            target,
            &[
                GpuBinding::Texture { binding: 0, texture: mask },
                GpuBinding::Sampler { binding: 1, sampler: &self.sampler },
                GpuBinding::Bytes { binding: 2, data: bytemuck::bytes_of(&uniforms) },
            ],
            viewport,
            manifold_gpu::GpuLoadAction::Load,
            "Selection Outline",
        );
    }
}

/// The content thread's outline state: which render slots each picture
/// outlines, the layers that request was applied to, and the two mask
/// bridges (the output preview's, and the scene viewport's).
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(crate) struct OutlineCapture {
    output: Vec<u32>,
    viewport: Vec<u32>,
    output_layer: Option<manifold_core::LayerId>,
    viewport_layer: Option<manifold_core::LayerId>,
    /// A request changed since it was last applied.
    dirty: bool,
    pub output_masks: OutlineMasks,
    pub viewport_masks: OutlineMasks,
}

#[cfg(target_os = "macos")]
impl OutlineCapture {
    pub fn set(&mut self, output: &[u32], viewport: &[u32]) {
        if self.output != output || self.viewport != viewport {
            self.output.clear();
            self.output.extend_from_slice(output);
            self.viewport.clear();
            self.viewport.extend_from_slice(viewport);
            self.dirty = true;
        }
    }

    /// Before the generators render: aim the requests at `output_layer`'s
    /// scene and `viewport_layer`'s viewport, clearing a layer a request
    /// left. A rebuilt graph starts without a request, so a standing one is
    /// set again every frame.
    pub fn apply(
        &mut self,
        generators: &mut manifold_compositor::generator_renderer::GeneratorRenderer,
        output_layer: Option<&manifold_core::LayerId>,
        viewport_layer: Option<&manifold_core::LayerId>,
    ) {
        if self.output_layer.as_ref() != output_layer {
            if let Some(old) = self.output_layer.take() {
                generators.set_selection_mask(&old, &[]);
            }
            self.output_layer = output_layer.cloned();
            self.dirty = true;
        }
        if self.viewport_layer.as_ref() != viewport_layer {
            if let Some(old) = self.viewport_layer.take() {
                generators.set_scene_viewport_selection_mask(&old, &[]);
            }
            self.viewport_layer = viewport_layer.cloned();
            self.dirty = true;
        }
        if let Some(layer) = self.output_layer.as_ref()
            && (self.dirty || !self.output.is_empty())
        {
            generators.set_selection_mask(layer, &self.output);
        }
        if let Some(layer) = self.viewport_layer.as_ref()
            && (self.dirty || !self.viewport.is_empty())
        {
            generators.set_scene_viewport_selection_mask(layer, &self.viewport);
        }
        self.dirty = false;
    }
}

/// The mask beside surface `slot` of a picture whose frame stamped mask
/// generation `outline`, read under that picture's lease. `None` when the
/// frame drew no mask or the bridge has been resized since.
#[cfg(target_os = "macos")]
pub(crate) fn leased_mask(
    pane: Option<&mut crate::texture_pane::TexturePane>,
    device: &GpuDevice,
    slot: usize,
    outline: Option<u64>,
) -> Option<GpuTexture> {
    let pane = pane?;
    if pane.bridge()?.generation() != outline? {
        return None;
    }
    pane.leased(device, slot).cloned()
}

impl crate::app::Application {
    /// Send the outline's render slots when they move: the output preview's
    /// selected object while Show gizmos is on, and for the scene viewport
    /// the slots of whichever host owns it (D-99).
    pub(crate) fn sync_selection_outline(&mut self) {
        let output: &[u32] = if self.ws.ui_root.scene_setup_panel.gizmo_layer().is_some() { self.ws.preview_gizmo.outline() } else { &[] };
        let viewport: &[u32] = if self.ws.preview_navigation.active() {
            output
        } else {
            self.graph_editor.as_ref().filter(|ed| ed.viewport_session.is_some()).map_or(&[], |ed| &ed.viewport_outline)
        };
        if self.ws.outline_sent.0 == output && self.ws.outline_sent.1 == viewport {
            return;
        }
        self.ws.outline_sent = (output.to_vec(), viewport.to_vec());
        let (output, viewport) = self.ws.outline_sent.clone();
        self.send_content_cmd(crate::content_command::ContentCommand::SetSelectionOutline { output, viewport });
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_proof;
