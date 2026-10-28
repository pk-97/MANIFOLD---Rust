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
    pub fn install(&mut self, bridge: Arc<SharedTextureBridge>) {
        self.textures = std::array::from_fn(|i| Some(bridge.texture(i)));
        self.bridge = Some(bridge);
    }

    /// Copy this frame's `mask` into surface `slot`, resizing the bridge to
    /// it first. The bridge generation the frame records, or `None` when
    /// there is no mask.
    pub fn write(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        slot: usize,
        mask: Option<&GpuTexture>,
    ) -> Option<u64> {
        let (mask, bridge) = (mask?, self.bridge.as_ref()?);
        if bridge.width() != mask.width || bridge.height() != mask.height {
            bridge.resize(device, mask.width, mask.height);
            self.textures = std::array::from_fn(|i| Some(bridge.texture(i)));
        }
        enc.copy_texture_to_texture(
            mask,
            self.textures[slot].as_ref()?,
            mask.width,
            mask.height,
            1,
        );
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
                &manifold_gpu::RenderPipelineDesc::new(
                    OUTLINE_WGSL,
                    "vs_main",
                    "fs_main",
                    "Selection Outline",
                )
                .color(
                    manifold_compositor::presentation::UI_FORMAT,
                    Some(premultiplied),
                ),
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
    pub fn draw(
        &self,
        encoder: &mut GpuEncoder,
        target: &GpuTexture,
        mask: &GpuTexture,
        viewport: (f32, f32, f32, f32),
        scale: f32,
    ) {
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
        encoder.draw(
            &manifold_gpu::DrawPass::color(target, manifold_gpu::GpuLoadAction::Load)
                .viewport(viewport),
            &[manifold_gpu::Draw::fullscreen(
                &self.pipeline,
                &[
                    GpuBinding::Texture {
                        binding: 0,
                        texture: mask,
                    },
                    GpuBinding::Sampler {
                        binding: 1,
                        sampler: &self.sampler,
                    },
                    GpuBinding::Bytes {
                        binding: 2,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                ],
            )],
            "Selection Outline",
        );
    }
}

/// The render slots one picture outlines and the scene layer they were read
/// from. They apply to that layer only, so a layer switch never outlines
/// whatever sits in the same slot of the next scene.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OutlineSlots {
    pub layer: manifold_core::LayerId,
    pub slots: Vec<u32>,
}

/// Where the outline requests land: the output render's selection mask and
/// the scene viewport pass's, per layer.
#[cfg(target_os = "macos")]
pub(crate) trait SelectionMasks {
    fn set_output_mask(&mut self, layer: &manifold_core::LayerId, slots: &[u32]);
    fn set_viewport_mask(&mut self, layer: &manifold_core::LayerId, slots: &[u32]);
}

#[cfg(target_os = "macos")]
impl SelectionMasks for manifold_compositor::generator_renderer::GeneratorRenderer {
    fn set_output_mask(&mut self, layer: &manifold_core::LayerId, slots: &[u32]) {
        self.set_selection_mask(layer, slots);
    }

    fn set_viewport_mask(&mut self, layer: &manifold_core::LayerId, slots: &[u32]) {
        self.set_scene_viewport_selection_mask(layer, slots);
    }
}

/// The content thread's outline state: the standing requests for the output
/// and the scene viewport, the layers they were last applied to, and the
/// two mask bridges (the output preview's, and the scene viewport's).
#[cfg(target_os = "macos")]
#[derive(Default)]
pub(crate) struct OutlineCapture {
    output: Option<OutlineSlots>,
    viewport: Option<OutlineSlots>,
    output_layer: Option<manifold_core::LayerId>,
    viewport_layer: Option<manifold_core::LayerId>,
    /// A request changed since it was last applied.
    dirty: bool,
    pub output_masks: OutlineMasks,
    pub viewport_masks: OutlineMasks,
}

#[cfg(target_os = "macos")]
impl OutlineCapture {
    pub fn set(&mut self, output: Option<OutlineSlots>, viewport: Option<OutlineSlots>) {
        if self.output != output || self.viewport != viewport {
            self.output = output;
            self.viewport = viewport;
            self.dirty = true;
        }
    }

    /// Withdraw both requests, returning them for [`Self::set`] later. The
    /// next [`Self::apply`] clears both masks.
    pub fn take(&mut self) -> (Option<OutlineSlots>, Option<OutlineSlots>) {
        self.dirty = true;
        (self.output.take(), self.viewport.take())
    }

    /// Before the generators render: aim each request at its layer when
    /// that layer is the one shown (`output_layer`'s scene, `viewport_layer`'s
    /// viewport), clearing a layer the picture left. A rebuilt graph starts
    /// without a request, so a standing one is set again every frame.
    pub fn apply(
        &mut self,
        masks: &mut impl SelectionMasks,
        output_layer: Option<&manifold_core::LayerId>,
        viewport_layer: Option<&manifold_core::LayerId>,
    ) {
        if self.output_layer.as_ref() != output_layer {
            if let Some(old) = self.output_layer.take() {
                masks.set_output_mask(&old, &[]);
            }
            self.output_layer = output_layer.cloned();
            self.dirty = true;
        }
        if self.viewport_layer.as_ref() != viewport_layer {
            if let Some(old) = self.viewport_layer.take() {
                masks.set_viewport_mask(&old, &[]);
            }
            self.viewport_layer = viewport_layer.cloned();
            self.dirty = true;
        }
        if let Some(layer) = self.output_layer.as_ref() {
            let slots = slots_on(self.output.as_ref(), layer);
            if self.dirty || !slots.is_empty() {
                masks.set_output_mask(layer, slots);
            }
        }
        if let Some(layer) = self.viewport_layer.as_ref() {
            let slots = slots_on(self.viewport.as_ref(), layer);
            if self.dirty || !slots.is_empty() {
                masks.set_viewport_mask(layer, slots);
            }
        }
        self.dirty = false;
    }
}

/// The slots `request` asks of `layer`: none when it was read from another.
#[cfg(target_os = "macos")]
fn slots_on<'a>(request: Option<&'a OutlineSlots>, layer: &manifold_core::LayerId) -> &'a [u32] {
    request
        .filter(|request| &request.layer == layer)
        .map_or(&[], |request| &request.slots)
}

/// The mask beside surface `slot` of a picture whose frame stamped mask
/// generation `outline`, read under that picture's lease. `None` when the
/// frame drew no mask or the bridge has been resized since.
#[cfg(target_os = "macos")]
pub(crate) fn leased_mask(
    pane: Option<&mut crate::texture_pane::TexturePane>,
    slot: usize,
    outline: Option<u64>,
) -> Option<GpuTexture> {
    let pane = pane?;
    if pane.bridge()?.generation() != outline? {
        return None;
    }
    pane.leased(slot).cloned()
}

impl crate::app::Application {
    /// Send the outline's render slots when they move: the output preview's
    /// selected object while Show gizmos is on, and for the scene viewport
    /// the slots of whichever host owns it (D-99).
    pub(crate) fn sync_selection_outline(&mut self) {
        let gizmo_layer = self.ws.ui_root.scene_setup_panel.gizmo_layer();
        let output = gizmo_layer.map(|layer| (layer, self.ws.preview_gizmo.outline()));
        let viewport = if self.ws.preview_navigation.active() {
            output
        } else {
            let editor_layer = match self.watched_graph_target.as_ref() {
                Some(manifold_core::GraphTarget::Generator(layer)) => Some(layer),
                _ => None,
            };
            self.graph_editor
                .as_ref()
                .filter(|ed| ed.viewport_session.is_some())
                .zip(editor_layer)
                .map(|(ed, layer)| (layer, ed.viewport_outline.as_slice()))
        };
        let (sent_output, sent_viewport) = &self.ws.outline_sent;
        if same_request(sent_output.as_ref(), output)
            && same_request(sent_viewport.as_ref(), viewport)
        {
            return;
        }
        let request = |wanted: Option<(&manifold_core::LayerId, &[u32])>| {
            wanted
                .filter(|(_, slots)| !slots.is_empty())
                .map(|(layer, slots)| OutlineSlots {
                    layer: layer.clone(),
                    slots: slots.to_vec(),
                })
        };
        self.ws.outline_sent = (request(output), request(viewport));
        let (output, viewport) = self.ws.outline_sent.clone();
        self.send_content_cmd(
            crate::content_command::ContentCommand::SetSelectionOutline { output, viewport },
        );
    }
}

/// `sent` is the request `wanted` makes; no slots is no request.
fn same_request(
    sent: Option<&OutlineSlots>,
    wanted: Option<(&manifold_core::LayerId, &[u32])>,
) -> bool {
    match (sent, wanted.filter(|(_, slots)| !slots.is_empty())) {
        (None, None) => true,
        (Some(sent), Some((layer, slots))) => &sent.layer == layer && sent.slots == slots,
        _ => false,
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_proof;

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use manifold_core::LayerId;

    use super::*;

    /// Every mask request `apply` makes, in order: (viewport?, layer, slots).
    #[derive(Default)]
    struct Recorded(Vec<(bool, LayerId, Vec<u32>)>);

    impl SelectionMasks for Recorded {
        fn set_output_mask(&mut self, layer: &LayerId, slots: &[u32]) {
            self.0.push((false, layer.clone(), slots.to_vec()));
        }

        fn set_viewport_mask(&mut self, layer: &LayerId, slots: &[u32]) {
            self.0.push((true, layer.clone(), slots.to_vec()));
        }
    }

    fn slots(layer: &LayerId, slots: &[u32]) -> Option<OutlineSlots> {
        Some(OutlineSlots {
            layer: layer.clone(),
            slots: slots.to_vec(),
        })
    }

    fn applied(
        capture: &mut OutlineCapture,
        output: Option<&LayerId>,
        viewport: Option<&LayerId>,
    ) -> Vec<(bool, LayerId, Vec<u32>)> {
        let mut masks = Recorded::default();
        capture.apply(&mut masks, output, viewport);
        masks.0
    }

    #[test]
    fn apply_aims_each_request_at_its_own_layer_only() {
        let (a, b) = (LayerId::new("a"), LayerId::new("b"));
        let mut capture = OutlineCapture::default();
        capture.set(slots(&a, &[2]), slots(&a, &[2]));
        assert_eq!(
            applied(&mut capture, Some(&a), Some(&a)),
            [(false, a.clone(), vec![2]), (true, a.clone(), vec![2])]
        );
        assert_eq!(
            applied(&mut capture, Some(&a), Some(&a)).len(),
            2,
            "a standing request is set every frame"
        );

        // The picture moves to layer b before the UI has read b's selection:
        // a is cleared, and b's scene gets nothing from a's slots.
        assert_eq!(
            applied(&mut capture, Some(&b), None),
            [
                (false, a.clone(), vec![]),
                (true, a.clone(), vec![]),
                (false, b.clone(), vec![])
            ],
        );
        assert!(
            applied(&mut capture, Some(&b), None).is_empty(),
            "nothing standing, nothing sent"
        );

        capture.set(slots(&b, &[0, 3]), None);
        assert_eq!(
            applied(&mut capture, Some(&b), None),
            [(false, b.clone(), vec![0, 3])]
        );
        capture.set(None, None);
        assert_eq!(
            applied(&mut capture, Some(&b), None),
            [(false, b.clone(), vec![])],
            "a deselect clears the mask"
        );
        assert!(applied(&mut capture, Some(&b), None).is_empty());
    }

    /// An export takes the requests out: its first frame clears both masks
    /// and the next ones send nothing; putting them back resumes them.
    #[test]
    fn a_taken_request_clears_the_masks_until_it_is_back() {
        let a = LayerId::new("a");
        let mut capture = OutlineCapture::default();
        capture.set(slots(&a, &[1]), slots(&a, &[4]));
        applied(&mut capture, Some(&a), Some(&a));
        let (output, viewport) = capture.take();
        assert_eq!(
            (output.clone(), viewport.clone()),
            (slots(&a, &[1]), slots(&a, &[4]))
        );
        assert_eq!(
            applied(&mut capture, Some(&a), Some(&a)),
            [(false, a.clone(), vec![]), (true, a.clone(), vec![])]
        );
        assert!(applied(&mut capture, Some(&a), Some(&a)).is_empty());
        capture.set(output, viewport);
        assert_eq!(
            applied(&mut capture, Some(&a), Some(&a)),
            [(false, a.clone(), vec![1]), (true, a.clone(), vec![4])]
        );
    }
}
