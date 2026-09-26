//! Typed linear-scene presentation for SDR and EDR destinations.

use manifold_core::TonemapCurve;
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuEncoder, GpuLoadAction, GpuRenderPipeline, GpuSampler,
    GpuSamplerDesc, GpuTexture, GpuTextureFormat,
};

/// Float format used by every live UI presentation destination.
pub const UI_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

/// Validated possible display headroom, in SDR white units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PotentialHeadroom(f64);

impl PotentialHeadroom {
    pub fn new(value: f64) -> Result<Self, String> {
        if value.is_finite() && value >= 1.0 {
            Ok(Self(value))
        } else {
            Err("display potential headroom must be finite and at least 1".into())
        }
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Validated currently usable display headroom, in SDR white units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurrentHeadroom(f64);

impl CurrentHeadroom {
    pub fn new(value: f64) -> Result<Self, String> {
        if value.is_finite() && value >= 1.0 {
            Ok(Self(value))
        } else {
            Err("display current headroom must be finite and at least 1".into())
        }
    }

    pub fn value(self) -> f64 {
        self.0
    }
}

/// Capabilities for one display destination.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayCapabilities {
    potential: PotentialHeadroom,
    current: CurrentHeadroom,
}

impl DisplayCapabilities {
    pub fn new(potential: PotentialHeadroom, current: CurrentHeadroom) -> Self {
        Self { potential, current }
    }

    pub fn sdr() -> Self {
        Self {
            potential: PotentialHeadroom(1.0),
            current: CurrentHeadroom(1.0),
        }
    }

    pub fn current(self) -> CurrentHeadroom {
        self.current
    }

    pub fn potential(self) -> PotentialHeadroom {
        self.potential
    }
}

/// A display destination with independent capability state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisplayDestination {
    Workspace,
    Output,
    GraphEditor,
}

/// Per-destination display capability state. This is content-thread owned and
/// deliberately contains no locks or heap-backed maps.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayPresentationState {
    workspace: Option<DisplayCapabilities>,
    output: Option<DisplayCapabilities>,
    graph_editor: Option<DisplayCapabilities>,
}

impl Default for DisplayPresentationState {
    fn default() -> Self {
        Self {
            workspace: Some(DisplayCapabilities::sdr()),
            output: None,
            graph_editor: Some(DisplayCapabilities::sdr()),
        }
    }
}

impl DisplayPresentationState {
    pub fn update(
        &mut self,
        destination: DisplayDestination,
        capabilities: DisplayCapabilities,
    ) -> bool {
        let slot = self.slot_mut(destination);
        let changed = *slot != Some(capabilities);
        *slot = Some(capabilities);
        changed
    }

    pub fn remove(&mut self, destination: DisplayDestination) -> bool {
        self.slot_mut(destination).take().is_some()
    }

    pub fn capabilities(&self, destination: DisplayDestination) -> Option<DisplayCapabilities> {
        self.slot(destination)
    }

    pub fn plan(
        &self,
        destination: DisplayDestination,
        curve: TonemapCurve,
    ) -> Option<DisplayPlan> {
        self.capabilities(destination)
            .map(|capabilities| DisplayPlan::new(capabilities, curve))
    }

    fn slot(&self, destination: DisplayDestination) -> Option<DisplayCapabilities> {
        match destination {
            DisplayDestination::Workspace => self.workspace,
            DisplayDestination::Output => self.output,
            DisplayDestination::GraphEditor => self.graph_editor,
        }
    }

    fn slot_mut(&mut self, destination: DisplayDestination) -> &mut Option<DisplayCapabilities> {
        match destination {
            DisplayDestination::Workspace => &mut self.workspace,
            DisplayDestination::Output => &mut self.output,
            DisplayDestination::GraphEditor => &mut self.graph_editor,
        }
    }
}

/// Immutable per-frame mapping policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DisplayPlan {
    current: CurrentHeadroom,
    curve: TonemapCurve,
}

impl DisplayPlan {
    pub fn new(capabilities: DisplayCapabilities, curve: TonemapCurve) -> Self {
        Self {
            current: capabilities.current,
            curve,
        }
    }
}

/// A canonical float RGBA scene source. Explicit colour previews use
/// [`LinearSceneFrame::from_color_texture`] to establish their semantics.
pub struct LinearSceneFrame<'a> {
    source: &'a GpuTexture,
}

impl<'a> LinearSceneFrame<'a> {
    pub fn new(source: &'a GpuTexture) -> Result<Self, String> {
        match source.format {
            GpuTextureFormat::Rgba16Float | GpuTextureFormat::Rgba32Float => Ok(Self { source }),
            format => Err(format!(
                "linear scene frame requires RGBA16Float or RGBA32Float, got {format:?}"
            )),
        }
    }

    /// Construct a scene frame from a graph colour preview whose producer has
    /// established that the pixels are colour data. UNORM and sRGB formats are
    /// accepted only through this explicit semantic boundary; scalar, depth,
    /// and other data textures remain rejected by `new`.
    pub fn from_color_texture(source: &'a GpuTexture) -> Result<Self, String> {
        match source.format {
            GpuTextureFormat::Rgba16Float
            | GpuTextureFormat::Rgba32Float
            | GpuTextureFormat::Rgba8Unorm
            | GpuTextureFormat::Bgra8Unorm
            | GpuTextureFormat::Rgba8UnormSrgb
            | GpuTextureFormat::Bgra8UnormSrgb => Ok(Self { source }),
            format => Err(format!(
                "colour preview requires an RGBA/BGRA colour texture, got {format:?}"
            )),
        }
    }

    fn texture(&self) -> &'a GpuTexture {
        self.source
    }
}

/// A float RGBA destination for display presentation.
pub struct LinearPresentationTarget<'a> {
    target: &'a GpuTexture,
}

impl<'a> LinearPresentationTarget<'a> {
    pub fn new(target: &'a GpuTexture) -> Result<Self, String> {
        if target.format == UI_FORMAT {
            Ok(Self { target })
        } else {
            Err(format!(
                "linear presentation target requires {UI_FORMAT:?}, got {:?}",
                target.format
            ))
        }
    }

    fn texture(&self) -> &'a GpuTexture {
        self.target
    }
}

/// Result of one encoded display mapping.
pub struct DisplayMappedFrame<'a> {
    target: LinearPresentationTarget<'a>,
}

impl<'a> DisplayMappedFrame<'a> {
    pub fn texture(&self) -> &GpuTexture {
        self.target.texture()
    }
}

/// Allocation-free fullscreen presentation mapper.
pub struct PresentationPipeline {
    pipeline: GpuRenderPipeline,
    sampler: GpuSampler,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PresentationUniforms {
    exposure: f32,
    paper_white: f32,
    current_headroom: f32,
    mode: u32,
    curve: u32,
    _pad0: f32,
    _pad1: f32,
    _pad2: f32,
}

impl PresentationPipeline {
    pub fn new(device: &GpuDevice) -> Self {
        let pipeline = device.create_render_pipeline(
            concat!(
                include_str!("effects/shaders/tonemap_common.wgsl"),
                include_str!("effects/shaders/presentation.wgsl")
            ),
            "vs_main",
            "fs_main",
            UI_FORMAT,
            None,
            "Linear Display Presentation",
        );
        let sampler = device.create_sampler(&GpuSamplerDesc::default());
        Self { pipeline, sampler }
    }

    pub fn encode<'a>(
        &self,
        encoder: &mut GpuEncoder,
        source: LinearSceneFrame<'_>,
        target: LinearPresentationTarget<'a>,
        plan: DisplayPlan,
        viewport: (f32, f32, f32, f32),
        load: GpuLoadAction,
    ) -> DisplayMappedFrame<'a> {
        let uniforms = PresentationUniforms {
            exposure: 1.0,
            paper_white: 1.0,
            current_headroom: plan.current.value() as f32,
            mode: u32::from(plan.current.value() > 1.0),
            curve: plan.curve as u32,
            _pad0: 0.0,
            _pad1: 0.0,
            _pad2: 0.0,
        };
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: source.texture(),
            },
            GpuBinding::Sampler {
                binding: 2,
                sampler: &self.sampler,
            },
        ];
        encoder.draw_fullscreen_viewport(
            &self.pipeline,
            target.texture(),
            &bindings,
            viewport,
            load,
            "Linear Display Presentation",
        );
        DisplayMappedFrame { target }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_keeps_destinations_independent() {
        let mut state = DisplayPresentationState::default();
        let hdr = DisplayCapabilities::new(
            PotentialHeadroom::new(3.0).unwrap(),
            CurrentHeadroom::new(2.0).unwrap(),
        );
        assert!(state.update(DisplayDestination::Output, hdr));
        assert_eq!(
            state.capabilities(DisplayDestination::Workspace),
            Some(DisplayCapabilities::sdr())
        );
        assert_eq!(state.capabilities(DisplayDestination::Output), Some(hdr));
        assert!(state.remove(DisplayDestination::Output));
        assert_eq!(
            state.capabilities(DisplayDestination::Workspace),
            Some(DisplayCapabilities::sdr())
        );
    }

    #[test]
    fn capabilities_use_current_headroom_for_plan() {
        let capabilities = DisplayCapabilities::new(
            PotentialHeadroom::new(4.0).unwrap(),
            CurrentHeadroom::new(1.5).unwrap(),
        );
        let plan = DisplayPlan::new(capabilities, TonemapCurve::AcesNarkowicz);
        assert_eq!(plan.current.value(), 1.5);
    }

    #[test]
    fn headroom_rejects_invalid_values() {
        assert!(CurrentHeadroom::new(f64::NAN).is_err());
        assert!(CurrentHeadroom::new(f64::INFINITY).is_err());
        assert!(CurrentHeadroom::new(0.99).is_err());
        assert!(PotentialHeadroom::new(-1.0).is_err());
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests {
    use super::*;
    use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
    use crate::tonemap::{TonemapMode, TonemapPipeline, TonemapSettings};
    use half::f16;
    use manifold_core::TonemapCurve;
    use manifold_gpu::{GpuTextureDesc, GpuTextureDimension, GpuTextureUsage};

    const W: u32 = 1;
    const H: u32 = 1;
    const ROW_BYTES: u32 = 256;

    fn make_texture(
        device: &manifold_gpu::GpuDevice,
        format: GpuTextureFormat,
        usage: GpuTextureUsage,
        label: &str,
    ) -> GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format,
            dimension: GpuTextureDimension::D2,
            usage,
            label,
            mip_levels: 1,
        })
    }

    fn pixel_bytes(pixel: [f32; 4]) -> [u8; 8] {
        let mut bytes = [0u8; 8];
        for (index, value) in pixel.into_iter().enumerate() {
            bytes[index * 2..index * 2 + 2]
                .copy_from_slice(&f16::from_f32(value).to_bits().to_le_bytes());
        }
        bytes
    }

    fn read_pixel(buffer: &manifold_gpu::GpuBuffer) -> [f32; 4] {
        let ptr = buffer
            .mapped_ptr()
            .expect("shared readback buffer must be mapped");
        let bytes = unsafe { std::slice::from_raw_parts(ptr, ROW_BYTES as usize) };
        std::array::from_fn(|index| {
            let offset = index * 2;
            f16::from_bits(u16::from_le_bytes([bytes[offset], bytes[offset + 1]])).to_f32()
        })
    }

    fn run_presentation(
        device: &manifold_gpu::GpuDevice,
        input: [f32; 4],
        capabilities: DisplayCapabilities,
        curve: TonemapCurve,
    ) -> [f32; 4] {
        let source = make_texture(
            device,
            GpuTextureFormat::Rgba16Float,
            GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            "presentation-proof-source",
        );
        let target = make_texture(
            device,
            UI_FORMAT,
            GpuTextureUsage::RENDER_TARGET_FULL,
            "presentation-proof-target",
        );
        let readback = device.create_buffer_shared(ROW_BYTES as u64);
        let pipeline = PresentationPipeline::new(device);
        let plan = DisplayPlan::new(capabilities, curve);
        let mut enc = device.create_encoder("presentation-proof");
        enc.upload_texture(&source, W, H, 1, &pixel_bytes(input));
        pipeline.encode(
            &mut enc,
            LinearSceneFrame::new(&source).unwrap(),
            LinearPresentationTarget::new(&target).unwrap(),
            plan,
            (0.0, 0.0, W as f32, H as f32),
            GpuLoadAction::Clear,
        );
        enc.copy_texture_to_buffer(&target, &readback, W, H, ROW_BYTES);
        enc.commit_and_wait_completed();
        read_pixel(&readback)
    }

    fn run_tonemap_mode(
        device: &manifold_gpu::GpuDevice,
        input: [f32; 4],
        mode: TonemapMode,
    ) -> [f32; 4] {
        let source = make_texture(
            device,
            GpuTextureFormat::Rgba16Float,
            GpuTextureUsage::RENDER_TARGET_FULL | GpuTextureUsage::CPU_UPLOAD,
            "scene-linear-proof-source",
        );
        let tonemap = TonemapPipeline::new(device, W, H);
        let readback = device.create_buffer_shared(ROW_BYTES as u64);
        let mut native = device.create_encoder("scene-linear-proof");
        enc_upload(&mut native, &source, input);
        {
            let mut renderer = RendererGpuEncoder::new(&mut native, device);
            tonemap.apply(
                &mut renderer,
                &source,
                &TonemapSettings {
                    mode,
                    ..TonemapSettings::default()
                },
            );
        }
        native.copy_texture_to_buffer(&tonemap.output.texture, &readback, W, H, ROW_BYTES);
        native.commit_and_wait_completed();
        read_pixel(&readback)
    }

    fn enc_upload(enc: &mut manifold_gpu::GpuEncoder, texture: &GpuTexture, pixel: [f32; 4]) {
        enc.upload_texture(texture, W, H, 1, &pixel_bytes(pixel));
    }

    fn sdr() -> DisplayCapabilities {
        DisplayCapabilities::sdr()
    }

    #[test]
    fn presentation_preserves_hdr_values_under_current_headroom() {
        let device = crate::test_device();
        let caps = DisplayCapabilities::new(
            PotentialHeadroom::new(8.0).unwrap(),
            CurrentHeadroom::new(4.0).unwrap(),
        );
        let out = run_presentation(
            &device,
            [2.0, 1.5, 0.5, 1.0],
            caps,
            TonemapCurve::AcesNarkowicz,
        );
        assert!(
            out[0] > 1.9 && out[0] < 2.1,
            "red HDR value was clipped: {out:?}"
        );
        assert!(
            out[1] > 1.4 && out[1] < 1.6,
            "green HDR value was clipped: {out:?}"
        );
    }

    #[test]
    fn presentation_sdr_uses_current_not_potential_headroom() {
        let device = crate::test_device();
        let caps = DisplayCapabilities::new(
            PotentialHeadroom::new(8.0).unwrap(),
            CurrentHeadroom::new(1.0).unwrap(),
        );
        let out = run_presentation(
            &device,
            [2.0, 2.0, 2.0, 1.0],
            caps,
            TonemapCurve::AcesNarkowicz,
        );
        assert!(
            out[0] <= 1.0 && out[0] > 0.0,
            "SDR output escaped bounds: {out:?}"
        );
        assert_eq!(out[0], out[1]);
    }

    #[test]
    fn presentation_sdr_preserves_authored_colour_without_an_extra_curve() {
        let device = crate::test_device();
        for curve in [
            TonemapCurve::AcesNarkowicz,
            TonemapCurve::AcesHill,
            TonemapCurve::Agx,
            TonemapCurve::KhronosPbrNeutral,
        ] {
            let presentation = run_presentation(&device, [1.7, 0.6, 0.2, 0.75], sdr(), curve);
            // Legacy SDR encoding of the linear HDR image: clip highlights,
            // preserve midtones, colour ratios below white, and alpha.
            let expected = [1.0, 0.6, 0.2, 0.75];
            for channel in 0..4 {
                assert!(
                    (presentation[channel] - expected[channel]).abs() < 0.002,
                    "{curve:?} channel {channel} differs: presentation={presentation:?} expected={expected:?}"
                );
            }
        }
    }

    #[test]
    fn presentation_keeps_sdr_white_stable_as_headroom_crosses_one() {
        let device = crate::test_device();
        for headroom in [1.0, 1.001, 1.01, 1.1, 1.25, 2.0] {
            let caps = DisplayCapabilities::new(
                PotentialHeadroom::new(4.0).unwrap(),
                CurrentHeadroom::new(headroom).unwrap(),
            );
            let input = [1.0, 0.9, 0.5, 0.75];
            let output = run_presentation(&device, input, caps, TonemapCurve::AcesNarkowicz);
            for channel in 0..4 {
                assert!(
                    (output[channel] - input[channel]).abs() < 0.001,
                    "SDR channel {channel} changed at headroom {headroom}: {output:?}"
                );
            }
        }
    }

    #[test]
    fn presentation_highlights_follow_headroom_without_a_reversal() {
        let device = crate::test_device();
        let input = [8.0, 2.0, 1.01, 0.75];
        let mut previous = [1.0; 3];
        for headroom in [1.0, 1.00001, 1.001, 1.01, 1.1, 1.249, 1.25, 1.251, 2.0, 4.0] {
            let caps = DisplayCapabilities::new(
                PotentialHeadroom::new(4.0).unwrap(),
                CurrentHeadroom::new(headroom).unwrap(),
            );
            let output = run_presentation(&device, input, caps, TonemapCurve::AcesNarkowicz);
            for channel in 0..3 {
                assert!(
                    output[channel].is_finite()
                        && output[channel] >= previous[channel] - 0.001
                        && output[channel] <= input[channel].min(headroom as f32) + 0.002,
                    "highlight {channel} reversed or exceeded headroom {headroom}: {output:?}, previous={previous:?}"
                );
                previous[channel] = output[channel];
            }
            assert_eq!(output[3], input[3]);
        }
    }

    #[test]
    fn presentation_preserves_distinct_low_float_values() {
        let device = crate::test_device();
        let caps = DisplayCapabilities::new(
            PotentialHeadroom::new(8.0).unwrap(),
            CurrentHeadroom::new(4.0).unwrap(),
        );
        let low = run_presentation(
            &device,
            [0.001, 0.001, 0.001, 1.0],
            caps,
            TonemapCurve::AcesNarkowicz,
        );
        let high = run_presentation(
            &device,
            [0.002, 0.002, 0.002, 1.0],
            caps,
            TonemapCurve::AcesNarkowicz,
        );
        assert!(
            low[0] > 0.0 && high[0] > low[0],
            "float shadow detail collapsed: low={low:?} high={high:?}"
        );
    }

    #[test]
    fn scene_linear_tonemap_preserves_hdr_values() {
        let device = crate::test_device();
        let out = run_tonemap_mode(&device, [2.0, 1.25, 0.5, 0.75], TonemapMode::SceneLinear);
        assert!((out[0] - 2.0).abs() < 0.01);
        assert!((out[1] - 1.25).abs() < 0.01);
        assert!((out[3] - 0.75).abs() < 0.01);
    }

    #[test]
    fn edr_tonemap_mode_keeps_the_existing_soft_shoulder() {
        let device = crate::test_device();
        let out = run_tonemap_mode(&device, [2.0, 1.25, 0.5, 1.0], TonemapMode::Edr);
        assert!(
            out[0] > 1.0 && out[0] < 2.01,
            "EDR mode lost highlight headroom: {out:?}"
        );
        assert!(out[1] > 1.0);
    }

    #[test]
    fn presentation_rejects_nonfloat_targets_and_nonfloat_linear_sources() {
        let device = crate::test_device();
        let target = make_texture(
            &device,
            GpuTextureFormat::Bgra8Unorm,
            GpuTextureUsage::RENDER_TARGET_FULL,
            "presentation-proof-unorm-target",
        );
        let source = make_texture(
            &device,
            GpuTextureFormat::Rgba8Unorm,
            GpuTextureUsage::RENDER_TARGET_FULL,
            "presentation-proof-unorm-source",
        );
        assert!(LinearPresentationTarget::new(&target).is_err());
        assert!(LinearSceneFrame::new(&source).is_err());
        assert!(LinearSceneFrame::from_color_texture(&source).is_ok());
    }
}
