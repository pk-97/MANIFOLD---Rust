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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuBlendOp {
    Add,
    Subtract,
    ReverseSubtract,
    Min,
    Max,
}

/// Blend state for a color attachment.
#[derive(Clone, Copy, Debug)]
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
}

// ─── Vertex Layout ───────────────────────────────────────────────────

/// Vertex attribute format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuVertexFormat {
    Float32,
    Float32x2,
    Float32x3,
    Float32x4,
    Uint32,
    Uint8x4,
}

/// A single vertex attribute in a vertex buffer layout.
#[derive(Clone, Copy, Debug)]
pub struct GpuVertexAttribute {
    /// Vertex format (e.g. Float32x2 for position).
    pub format: GpuVertexFormat,
    /// Byte offset within the vertex struct.
    pub offset: u32,
    /// Shader location (matches WGSL @location(N)).
    pub shader_location: u32,
}

/// Vertex buffer layout — describes the memory layout of vertices.
#[derive(Clone, Debug)]
pub struct GpuVertexLayout {
    /// Stride in bytes between consecutive vertices.
    pub stride: u32,
    /// Vertex attributes in this buffer.
    pub attributes: Vec<GpuVertexAttribute>,
}

// ─── Resource Bindings ───────────────────────────────────────────────

/// Resource binding for dispatch/draw calls.
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
