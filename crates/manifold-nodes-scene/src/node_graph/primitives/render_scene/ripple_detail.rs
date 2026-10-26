//! The ripple detail's slope tile: one tileable slope texture per water field,
//! made from the height field of the FFT ocean's spectrum.
//!
//! It runs only for a drawn field with a velocity lattice and a ripple detail
//! above zero; any other field allocates and dispatches nothing here. Each
//! frame the tile is rebuilt (spectrum, inverse FFT, slopes, mip chain) at the
//! frame's time, and `look_ripple_detail.wgsl` reads it through the water shade.
use manifold_gpu::*;

use super::super::ocean_spectrum::{OceanSpectrum, SpectrumUniforms};
use super::FieldObject;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

/// The FFT side: a power of two with 4 texels per cell across the 32 cells
/// the tile spans (`TILE_WAVELENGTHS` x `RESOLVED_WAVELENGTH_CELLS`). The
/// Ocean builds its own cascades at the spectrum's default 256.
const RIPPLE_TEXELS: u32 = 128;

/// Mip levels of the tile: `log2(RIPPLE_TEXELS) + 1`, down to one texel.
const MIP_LEVELS: u32 = RIPPLE_TEXELS.ilog2() + 1;

/// The shortest wave the field itself draws, in cells: a wave needs two cells
/// per half wavelength to survive the field build's smoothing, so the detail
/// starts below it.
const RESOLVED_WAVELENGTH_CELLS: f32 = 4.0;

/// The tile holds eight of the longest ripple, so its repeat is not seen.
const TILE_WAVELENGTHS: f32 = 8.0;

/// The flow map's cycle in seconds (Vlachos 2010, "Water Flow in Portal 2",
/// SIGGRAPH Advances in Real-Time Rendering: two layers half a cycle apart).
pub(super) const FLOW_PERIOD_SECONDS: f32 = 1.0;

/// Kelvin's minimum phase speed of capillary-gravity waves on clean water in
/// m/s, (4 g sigma / rho)^(1/4) with g 9.81 m/s², sigma 0.0728 N/m and rho
/// 998 kg/m³; slower flow cannot keep ripples. `look_ripple_detail.wgsl`
/// holds the same value.
#[cfg(test)]
pub(super) const MIN_RIPPLE_SPEED: f32 = 0.2313;

const SLOPES_WGSL: &str = include_str!("../shaders/ripple_slopes.wgsl");

/// Uniforms of `cs_slopes`, field for field with `SlopeUniforms` in `ripple_slopes.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct SlopeUniforms {
    n: u32,
    texel_metres: f32,
    _pad: [u32; 2],
}

/// The world size of the field's smallest cell: the lattice spacing on each
/// axis times the model's scale on that axis.
fn cell_metres(size: [f32; 3], nodes: [u32; 3], model: [[f32; 4]; 4]) -> f32 {
    (0..3)
        .map(|axis| {
            let column = model[axis];
            let scale =
                (column[0] * column[0] + column[1] * column[1] + column[2] * column[2]).sqrt();
            size[axis] / (nodes[axis] - 1) as f32 * scale
        })
        .fold(f32::MAX, f32::min)
}

/// The shortest wave the field itself draws, in metres.
fn resolved_metres(cell: f32) -> f32 {
    RESOLVED_WAVELENGTH_CELLS * cell
}

/// The tile's side in metres.
fn tile_metres(cell: f32) -> f32 {
    TILE_WAVELENGTHS * resolved_metres(cell)
}

/// The lowest wavenumber (rad/m) of the tile's spectrum: the field's own waves stay out.
fn band_low(cell: f32) -> f32 {
    std::f32::consts::TAU / resolved_metres(cell)
}

/// Whether a field wants ripples: a velocity lattice to flow along and a
/// strength above zero.
fn wants_ripples(has_velocity: bool, ripple_detail: f32) -> bool {
    has_velocity && ripple_detail > 0.0
}

fn field_cell(field: &FieldObject) -> f32 {
    cell_metres(field.size, field.nodes, field.model)
}

/// The tile's side in metres when the field wants ripples.
pub(super) fn tile_for(field: &FieldObject) -> Option<f32> {
    wants_ripples(field.velocity.is_some(), field.ripple_detail)
        .then(|| tile_metres(field_cell(field)))
}

/// One field's slope texture.
pub(super) struct RippleTile {
    slot: u32,
    pub(super) tile_metres: f32,
    pub(super) texture: GpuTexture,
}

#[derive(Default)]
pub(super) struct RippleDetail {
    spectrum_pipeline: Option<GpuComputePipeline>,
    slopes_pipeline: Option<GpuComputePipeline>,
    fft: Option<GpuFft>,
    spectrum: Option<GpuBuffer>,
    heights: Option<GpuBuffer>,
    sampler: Option<GpuSampler>,
    tiles: Vec<RippleTile>,
    dummy_slopes: Option<GpuTexture>,
    dummy_velocity: Option<GpuBuffer>,
}

impl RippleDetail {
    /// The sampler and the 1x1 stand-ins a field without ripples binds.
    fn ensure_common(&mut self, device: &GpuDevice) {
        if self.sampler.is_none() {
            self.sampler = Some(device.create_sampler(&GpuSamplerDesc {
                min_filter: GpuFilterMode::Linear,
                mag_filter: GpuFilterMode::Linear,
                mip_filter: GpuFilterMode::Linear,
                address_mode_u: GpuAddressMode::Repeat,
                address_mode_v: GpuAddressMode::Repeat,
                address_mode_w: GpuAddressMode::Repeat,
                lod_max_clamp: MIP_LEVELS as f32,
                ..Default::default()
            }));
        }
        if self.dummy_slopes.is_none() {
            self.dummy_slopes = Some(device.create_texture(&GpuTextureDesc {
                width: 1,
                height: 1,
                depth: 1,
                mip_levels: 1,
                format: GpuTextureFormat::Rg16Float,
                dimension: GpuTextureDimension::D2,
                usage: GpuTextureUsage::SHADER_READ,
                label: "ripple detail unused slopes",
            }));
        }
        if self.dummy_velocity.is_none() {
            self.dummy_velocity = Some(device.create_buffer(16));
        }
    }

    /// The pipelines, the FFT plan and the working buffers, on first use.
    fn ensure_compute(&mut self, device: &GpuDevice) {
        standalone_pipeline::<OceanSpectrum>(&mut self.spectrum_pipeline, device);
        if self.slopes_pipeline.is_none() {
            self.slopes_pipeline =
                Some(device.create_compute_pipeline(SLOPES_WGSL, "cs_slopes", "ripple slopes"));
        }
        if self.fft.is_none() {
            let n = RIPPLE_TEXELS as usize;
            self.fft = Some(GpuFft::new_nd(
                device,
                FftKind::HermiteanToReal,
                &[1, n, n],
                &[1, 2],
            ));
        }
        if self.spectrum.is_none() {
            let n = u64::from(RIPPLE_TEXELS);
            self.spectrum = Some(device.create_buffer(n * (n / 2 + 1) * 8));
            self.heights = Some(device.create_buffer(n * n * 4));
        }
    }

    fn new_texture(device: &GpuDevice) -> GpuTexture {
        device.create_texture(&GpuTextureDesc {
            width: RIPPLE_TEXELS,
            height: RIPPLE_TEXELS,
            depth: 1,
            mip_levels: MIP_LEVELS,
            format: GpuTextureFormat::Rg16Float,
            dimension: GpuTextureDimension::D2,
            // The blit mip generation needs a renderable format.
            usage: GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::SHADER_WRITE
                | GpuTextureUsage::RENDER_TARGET,
            label: "ripple detail slopes",
        })
    }

    /// Rebuild the slope tile of every field that wants ripples, and drop the
    /// tiles of fields that no longer do. Allocates only for a new slot.
    pub(super) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        fields: &[FieldObject],
        seconds: f32,
    ) {
        self.ensure_common(device);
        self.tiles.retain(|tile| {
            fields
                .iter()
                .any(|field| field.slot == tile.slot && tile_for(field).is_some())
        });
        for field in fields {
            let Some(side) = tile_for(field) else {
                continue;
            };
            self.ensure_compute(device);
            let index = match self.tiles.iter().position(|tile| tile.slot == field.slot) {
                Some(index) => index,
                None => {
                    self.tiles.push(RippleTile {
                        slot: field.slot,
                        tile_metres: side,
                        texture: Self::new_texture(device),
                    });
                    self.tiles.len() - 1
                }
            };
            self.tiles[index].tile_metres = side;
            let (
                Some(spectrum_pipeline),
                Some(slopes_pipeline),
                Some(fft),
                Some(spectrum),
                Some(heights),
            ) = (
                self.spectrum_pipeline.as_ref(),
                self.slopes_pipeline.as_ref(),
                self.fft.as_ref(),
                self.spectrum.as_ref(),
                self.heights.as_ref(),
            )
            else {
                unreachable!("ensure_compute built them");
            };
            let texture = &self.tiles[index].texture;

            let uniforms =
                SpectrumUniforms::ripple(RIPPLE_TEXELS, side, band_low(field_cell(field)), seconds);
            enc.dispatch_compute(
                spectrum_pipeline,
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::bytes_of(&uniforms),
                    },
                    GpuBinding::Buffer {
                        binding: 1,
                        buffer: spectrum,
                        offset: 0,
                    },
                ],
                [uniforms.dispatch_count.div_ceil(256), 1, 1],
                "ripple spectrum",
            );
            fft.encode(enc, spectrum, heights);
            encode_slopes(enc, slopes_pipeline, heights, texture, side);
            enc.generate_mipmaps(texture);
        }
    }

    /// The tile of the field in `slot`, if it has ripples this frame.
    /// Drop the tiles and the transform's buffers, so a frame with no water
    /// holds none of them; the pipelines and the FFT plan stay compiled.
    pub(super) fn reset(&mut self) {
        self.tiles.clear();
        self.spectrum = None;
        self.heights = None;
    }

    pub(super) fn tile(&self, slot: u32) -> Option<&RippleTile> {
        self.tiles.iter().find(|tile| tile.slot == slot)
    }

    /// The repeat, linear, linear-mip sampler of the tiles. `encode` made it.
    pub(super) fn sampler(&self) -> &GpuSampler {
        self.sampler.as_ref().expect("encode ran this frame")
    }

    /// The slopes texture a field without ripples binds. `encode` made it.
    pub(super) fn dummy_slopes(&self) -> &GpuTexture {
        self.dummy_slopes.as_ref().expect("encode ran this frame")
    }

    /// The velocity buffer a field without a lattice binds. `encode` made it.
    pub(super) fn dummy_velocity(&self) -> &GpuBuffer {
        self.dummy_velocity.as_ref().expect("encode ran this frame")
    }
}

/// Slopes of the `heights` tile of side `tile_metres` into `texture`'s first mip.
fn encode_slopes(
    enc: &mut GpuEncoder,
    pipeline: &GpuComputePipeline,
    heights: &GpuBuffer,
    texture: &GpuTexture,
    tile_metres: f32,
) {
    let uniforms = SlopeUniforms {
        n: RIPPLE_TEXELS,
        texel_metres: tile_metres / RIPPLE_TEXELS as f32,
        _pad: [0; 2],
    };
    enc.dispatch_compute(
        pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&uniforms),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: heights,
                offset: 0,
            },
            GpuBinding::Texture {
                binding: 2,
                texture,
            },
        ],
        [RIPPLE_TEXELS.div_ceil(16), RIPPLE_TEXELS.div_ceil(16), 1],
        "ripple slopes",
    );
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;

    const LOOK_WGSL: &str = concat!(
        include_str!("../shaders/look_ripple_detail.wgsl"),
        include_str!("../shaders/foam_field.wgsl"),
    );

    /// The look's `ripple_flow_strength`, in Rust.
    fn flow_strength(speed: f32) -> f32 {
        if speed <= MIN_RIPPLE_SPEED {
            0.0
        } else {
            1.0 - (MIN_RIPPLE_SPEED / speed).powi(2)
        }
    }

    #[test]
    fn tile_and_band_follow_the_cell() {
        let model = model_matrix([0.0; 3], [0.0; 3], [1.0; 3]);
        let cell = cell_metres([1.6; 3], [33; 3], model);
        assert!((cell - 0.05).abs() < 1e-6, "cell {cell}");
        assert!((tile_metres(cell) - 1.6).abs() < 1e-5);
        assert!((band_low(cell) - std::f32::consts::TAU / 0.2).abs() < 1e-3);

        let doubled = model_matrix([0.0; 3], [0.0; 3], [2.0; 3]);
        let cell = cell_metres([1.6; 3], [33; 3], doubled);
        assert!((tile_metres(cell) - 3.2).abs() < 1e-5);

        // The smallest cell on any axis sets the tile.
        let cell = cell_metres([1.6, 3.2, 1.6], [33, 33, 33], model);
        assert!((cell - 0.05).abs() < 1e-6, "cell {cell}");
        let cell = cell_metres(
            [1.6, 3.2, 1.6],
            [33, 33, 33],
            model_matrix([0.0; 3], [0.0; 3], [1.0, 0.25, 1.0]),
        );
        assert!((cell - 0.025).abs() < 1e-6, "cell {cell}");

        assert!(wants_ripples(true, 1.0));
        assert!(wants_ripples(true, 0.4));
        assert!(!wants_ripples(false, 1.0), "no velocity lattice");
        assert!(!wants_ripples(true, 0.0), "ripple detail off");
        assert!(!wants_ripples(true, f32::NAN));
    }

    #[test]
    fn flow_strength_starts_at_kelvins_minimum() {
        assert_eq!(flow_strength(0.2), 0.0);
        assert_eq!(flow_strength(MIN_RIPPLE_SPEED), 0.0);
        assert!((flow_strength(2.0 * MIN_RIPPLE_SPEED) - 0.75).abs() < 1e-6);

        let line = LOOK_WGSL
            .lines()
            .find(|line| line.starts_with("const MIN_RIPPLE_SPEED"))
            .expect("the look declares MIN_RIPPLE_SPEED");
        let value: f32 = line
            .split('=')
            .nth(1)
            .and_then(|rest| rest.trim().strip_suffix(';'))
            .expect("const value")
            .trim()
            .parse()
            .expect("a float literal");
        assert_eq!(value, MIN_RIPPLE_SPEED);
    }

    #[test]
    fn ripple_slopes_shader_validates() {
        let module = naga::front::wgsl::parse_str(SLOPES_WGSL)
            .expect("ripple slopes shader must naga-parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!(
                "ripple slopes validation failed:\n{}",
                e.emit_to_string(SLOPES_WGSL)
            );
        }
        assert!(module.entry_points.iter().any(|ep| ep.name == "cs_slopes"));
    }

    #[test]
    fn slope_uniforms_are_16_bytes() {
        assert_eq!(std::mem::size_of::<SlopeUniforms>(), 16);
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "ripple_detail_gpu_tests.rs"]
mod gpu_tests;

#[cfg(all(test, feature = "gpu-proofs"))]
#[path = "foam_gpu_tests.rs"]
mod foam_gpu_tests;
