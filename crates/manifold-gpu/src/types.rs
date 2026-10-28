//! Shared types for manifold-gpu — identical on all backends.

/// Texture formats used by MANIFOLD content thread.
/// Subset of Metal texture formats that we actually need.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuTextureFormat {
    Rgba16Float,
    Rgba32Float,
    Rgba8Unorm,
    R32Float,
    Rg32Float,
    R16Float,
    Rg16Float,
    R32Uint,
    Rgba8UnormSrgb,
    Bgra8Unorm,
    /// BGRA8 with hardware sRGB→linear decode on sample. Required by
    /// tv-led-mirror so the GPU's bilinear filter operates in linear space
    /// when sampling sRGB-encoded screen-capture IOSurfaces.
    Bgra8UnormSrgb,
    R8Unorm,
    /// 32-bit floating-point depth format for depth-stencil attachments.
    Depth32Float,
}

impl GpuTextureFormat {
    /// Bytes per pixel for this format.
    pub fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba16Float => 8,
            Self::Rgba32Float => 16,
            Self::Rgba8Unorm | Self::Rgba8UnormSrgb | Self::Bgra8Unorm | Self::Bgra8UnormSrgb => 4,
            Self::R32Float | Self::R32Uint => 4,
            Self::Rg32Float => 8,
            Self::R16Float => 2,
            Self::Rg16Float => 4,
            Self::R8Unorm => 1,
            Self::Depth32Float => 4,
        }
    }
}

/// Texture dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuTextureDimension {
    D2,
    D3,
    /// A stack of 2D layers; `GpuTextureDesc::depth` is the layer count.
    D2Array,
}

/// Texture usage flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct GpuTextureUsage(u32);

impl GpuTextureUsage {
    pub const RENDER_TARGET: Self = Self(1 << 0);
    pub const SHADER_READ: Self = Self(1 << 1);
    pub const SHADER_WRITE: Self = Self(1 << 2);
    pub const COPY_SRC: Self = Self(1 << 3);
    pub const COPY_DST: Self = Self(1 << 4);
    /// CPU-writable texture (uses Shared storage for replace_region upload).
    pub const CPU_UPLOAD: Self = Self(1 << 5);

    /// Standard content-thread render target usage.
    pub const RENDER_TARGET_FULL: Self = Self(
        Self::RENDER_TARGET.0
            | Self::SHADER_READ.0
            | Self::SHADER_WRITE.0
            | Self::COPY_SRC.0
            | Self::COPY_DST.0,
    );

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for GpuTextureUsage {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

/// Texture descriptor for creation.
pub struct GpuTextureDesc<'a> {
    pub width: u32,
    pub height: u32,
    pub depth: u32,
    pub format: GpuTextureFormat,
    pub dimension: GpuTextureDimension,
    pub usage: GpuTextureUsage,
    pub label: &'a str,
    /// Number of mipmap levels. 1 = no mipmaps (default). Higher values
    /// create a mipmap chain that can be populated with
    /// `GpuEncoder::generate_mipmaps` and sampled at any level via
    /// WGSL's `textureSampleLevel(tex, sampler, uv, mip)`. Mipmap-aware
    /// textures must be created with usages that allow both write
    /// (storage or render target) and shader read.
    pub mip_levels: u32,
}

impl<'a> GpuTextureDesc<'a> {
    /// Maximum number of mip levels for the given dimensions
    /// (1 + floor(log2(max(width, height)))).
    pub fn max_mip_levels(width: u32, height: u32) -> u32 {
        let m = width.max(height).max(1);
        32 - m.leading_zeros()
    }
}

/// Buffer storage mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuStorageMode {
    /// GPU-private (fastest for GPU-only access).
    Private,
    /// CPU+GPU shared memory (zero-copy uniform writes).
    Shared,
    /// Managed (explicit sync between CPU/GPU) — macOS only.
    Managed,
    /// Memoryless (Apple Silicon): data stays in tile/cache memory only.
    /// Zero VRAM bandwidth. Only valid for render pass attachments —
    /// NOT usable as storage textures in compute shaders.
    /// On macOS, only available on Apple Silicon (M1+).
    Memoryless,
}

/// Filter mode for samplers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuFilterMode {
    Nearest,
    Linear,
}

/// Address mode for samplers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuAddressMode {
    ClampToEdge,
    Repeat,
    MirrorRepeat,
    ClampToZero,
}

/// Sampler descriptor.
pub struct GpuSamplerDesc {
    pub min_filter: GpuFilterMode,
    pub mag_filter: GpuFilterMode,
    pub mip_filter: GpuFilterMode,
    pub address_mode_u: GpuAddressMode,
    pub address_mode_v: GpuAddressMode,
    pub address_mode_w: GpuAddressMode,
    /// Comparison function for depth comparison samplers (WGSL `sampler_comparison`).
    /// None for regular samplers.
    pub compare: Option<GpuCompareFunction>,
    /// Max anisotropic sample count. 1 = isotropic (the default; byte-identical
    /// to pre-field behavior). Metal: `setMaxAnisotropy` (1..=16).
    pub max_anisotropy: u32,
    /// Largest sampled mip level. Zero implements a non-mipmapped texture filter.
    pub lod_max_clamp: f32,
}

impl Default for GpuSamplerDesc {
    fn default() -> Self {
        Self {
            min_filter: GpuFilterMode::Linear,
            mag_filter: GpuFilterMode::Linear,
            mip_filter: GpuFilterMode::Nearest,
            address_mode_u: GpuAddressMode::ClampToEdge,
            address_mode_v: GpuAddressMode::ClampToEdge,
            address_mode_w: GpuAddressMode::ClampToEdge,
            compare: None,
            max_anisotropy: 1,
            lod_max_clamp: f32::MAX,
        }
    }
}

/// Load action for a render pass color attachment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuLoadAction {
    /// Clear to transparent black (0,0,0,0).
    Clear,
    /// Preserve existing contents (MTLLoadAction::Load).
    Load,
    /// Contents undefined — use when you'll write every pixel.
    DontCare,
}

/// Blend factor for render pipelines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuBlendFactor {
    Zero,
    One,
    SrcAlpha,
    OneMinusSrcAlpha,
    DstAlpha,
    OneMinusDstAlpha,
    SrcColor,
    OneMinusSrcColor,
    DstColor,
    OneMinusDstColor,
}

/// Blend operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuBlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// Blend state for a color attachment.
#[derive(Clone, Copy, Debug, Hash)]
pub struct GpuBlendState {
    pub src_factor: GpuBlendFactor,
    pub dst_factor: GpuBlendFactor,
    pub operation: GpuBlendOp,
    pub src_alpha_factor: GpuBlendFactor,
    pub dst_alpha_factor: GpuBlendFactor,
    pub alpha_operation: GpuBlendOp,
}

// ─── Depth-Stencil ──────────────────────────────────────────────────

/// Depth compare function for depth testing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuCompareFunction {
    Never,
    Less,
    Equal,
    LessEqual,
    Greater,
    NotEqual,
    GreaterEqual,
    Always,
}

/// Depth-stencil configuration for render pipelines.
#[derive(Clone, Copy, Debug)]
pub struct GpuDepthStencilDesc {
    pub compare: GpuCompareFunction,
    pub write_enabled: bool,
}

/// Triangle fill mode for render pipelines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuTriangleFillMode {
    /// Solid fill (default).
    Fill,
    /// Wireframe — draw triangle edges only.
    Lines,
}

/// Primitive type for draw calls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuPrimitiveType {
    Triangle,
    Line,
    /// Point sprites — one point per vertex, size from the vertex shader's
    /// point-size output (SCENE_RENDER_MODE_DESIGN.md D8; the pipeline is
    /// built via `GpuDevice::create_render_pipeline_depth_msaa_point_size`
    /// so the WGSL `@location` output is rebound to `BuiltIn::PointSize`).
    Point,
}

// ─── Vertex Layout ───────────────────────────────────────────────────

/// Vertex attribute format.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GpuVertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Uint32,
    Uint8x4,
}

/// A single vertex attribute in a vertex buffer layout.
#[derive(Clone, Copy, Debug, Hash)]
pub struct GpuVertexAttribute {
    /// Vertex format (e.g. Float32x2 for position).
    pub format: GpuVertexFormat,
    /// Byte offset within the vertex struct.
    pub offset: u32,
    /// Shader location (matches WGSL @location(N)).
    pub shader_location: u32,
}

/// Vertex buffer layout — describes the memory layout of vertices.
#[derive(Clone, Debug, Hash)]
pub struct GpuVertexLayout {
    /// Stride in bytes between consecutive vertices.
    pub stride: u32,
    /// Vertex attributes in this buffer.
    pub attributes: Vec<GpuVertexAttribute>,
}

// ─── Buffers ─────────────────────────────────────────────────────────

/// What the CPU does with a buffer, which decides where it lives. On unified
/// memory only `Gpu` differs (private storage); on a discrete GPU each use
/// gets its own memory type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BufferUse {
    /// Only the GPU reads and writes it. Never mapped; on a discrete GPU it
    /// sits in VRAM.
    Gpu,
    /// The CPU writes it, the GPU reads it. Mapped for writing: CPU reads
    /// from this memory are very slow on a discrete GPU.
    Upload,
    /// The GPU writes it, the CPU reads it back. Mapped and CPU-cached.
    Readback,
}

// ─── Render pipelines ────────────────────────────────────────────────

/// Everything a render pipeline is built from. Start from
/// [`RenderPipelineDesc::new`] (no colour, no depth, one sample) and set what
/// the pipeline needs; the backend panics on a combination it does not build.
#[derive(Clone, Copy, Debug)]
pub struct RenderPipelineDesc<'a> {
    pub wgsl_source: &'a str,
    pub vs_entry: &'a str,
    pub fs_entry: &'a str,
    pub label: &'a str,
    /// `None` for a depth-only pipeline, whose fragment writes no colour.
    pub color_format: Option<GpuTextureFormat>,
    pub blend: Option<GpuBlendState>,
    /// Extra colour attachments, indices 1.., for multiple render targets.
    pub aux_color_formats: &'a [GpuTextureFormat],
    pub depth_format: Option<GpuTextureFormat>,
    pub sample_count: u32,
    /// Turns fragment alpha, including a cutout `discard`, into per-sample
    /// coverage so the MSAA resolve smooths cutout edges.
    pub alpha_to_coverage: bool,
    /// The `vs_entry` output `@location` rebound to the point size (WGSL has
    /// no point-size builtin).
    pub point_size_location: Option<u32>,
    pub vertex_layout: Option<&'a GpuVertexLayout>,
    /// Text substitutions applied to `wgsl_source` before compiling: one
    /// WGSL template, several pipeline variants.
    pub specializations: &'a [(&'a str, &'a str)],
}

impl<'a> RenderPipelineDesc<'a> {
    pub fn new(wgsl_source: &'a str, vs_entry: &'a str, fs_entry: &'a str, label: &'a str) -> Self {
        Self {
            wgsl_source,
            vs_entry,
            fs_entry,
            label,
            color_format: None,
            blend: None,
            aux_color_formats: &[],
            depth_format: None,
            sample_count: 1,
            alpha_to_coverage: false,
            point_size_location: None,
            vertex_layout: None,
            specializations: &[],
        }
    }

    pub fn color(mut self, format: GpuTextureFormat, blend: Option<GpuBlendState>) -> Self {
        self.color_format = Some(format);
        self.blend = blend;
        self
    }

    pub fn depth(mut self, format: GpuTextureFormat) -> Self {
        self.depth_format = Some(format);
        self
    }

    pub fn samples(mut self, sample_count: u32) -> Self {
        self.sample_count = sample_count;
        self
    }

    pub fn alpha_to_coverage(mut self, enabled: bool) -> Self {
        self.alpha_to_coverage = enabled;
        self
    }

    pub fn point_size(mut self, location: u32) -> Self {
        self.point_size_location = Some(location);
        self
    }

    pub fn vertex_layout(mut self, layout: &'a GpuVertexLayout) -> Self {
        self.vertex_layout = Some(layout);
        self
    }

    pub fn aux_colors(mut self, formats: &'a [GpuTextureFormat]) -> Self {
        self.aux_color_formats = formats;
        self
    }

    pub fn specialize(mut self, specializations: &'a [(&'a str, &'a str)]) -> Self {
        self.specializations = specializations;
        self
    }
}

// ─── Resource Bindings ───────────────────────────────────────────────

/// Resource binding for dispatch/draw calls.
#[derive(Clone, Copy)]
pub enum GpuBinding<'a> {
    /// Buffer at WGSL @binding(N). Backend maps to Metal buffer index.
    Buffer {
        binding: u32,
        buffer: &'a super::GpuBuffer,
        offset: u64,
    },
    /// Texture at WGSL @binding(N). Backend maps to Metal texture index.
    Texture {
        binding: u32,
        texture: &'a super::GpuTexture,
    },
    /// Sampler at WGSL @binding(N). Backend maps to Metal sampler index.
    Sampler {
        binding: u32,
        sampler: &'a super::GpuSampler,
    },
    /// Inline bytes at WGSL @binding(N). Uses set_bytes on Metal (no buffer allocation).
    Bytes { binding: u32, data: &'a [u8] },
}

impl GpuBinding<'_> {
    /// The WGSL @binding(N) this binds.
    pub fn binding(&self) -> u32 {
        match *self {
            Self::Buffer { binding, .. }
            | Self::Texture { binding, .. }
            | Self::Sampler { binding, .. }
            | Self::Bytes { binding, .. } => binding,
        }
    }
}

/// The transform a [`GpuFft`](crate::GpuFft) plan computes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FftKind {
    /// Real input → half spectrum (interleaved complex), unscaled.
    RealToHermitean,
    /// Half spectrum → real output, inverse sign, scaled by 1 / volume.
    HermiteanToReal,
    /// Interleaved complex input and output, unscaled.
    ComplexToComplex { inverse: bool },
}

/// A copy of the manager's accounting, safe to pass across thread boundaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GpuResidencyStats {
    pub allocation_count: usize,
    pub allocated_bytes: u64,
    pub budget_bytes: u64,
    pub requested: bool,
}
