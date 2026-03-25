//! Shared types for manifold-gpu — identical on all backends.

/// Texture formats used by MANIFOLD content thread.
/// Subset of Metal/wgpu formats that we actually need.
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
}

/// Texture dimension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpuTextureDimension {
    D2,
    D3,
}

/// Texture usage flags.
#[derive(Clone, Copy, Debug)]
pub struct GpuTextureUsage(u32);

impl GpuTextureUsage {
    pub const RENDER_TARGET: Self = Self(1 << 0);
    pub const SHADER_READ: Self = Self(1 << 1);
    pub const SHADER_WRITE: Self = Self(1 << 2);
    pub const COPY_SRC: Self = Self(1 << 3);
    pub const COPY_DST: Self = Self(1 << 4);

    /// Standard content-thread render target usage.
    pub const RENDER_TARGET_FULL: Self =
        Self(Self::RENDER_TARGET.0 | Self::SHADER_READ.0 | Self::SHADER_WRITE.0
            | Self::COPY_SRC.0 | Self::COPY_DST.0);

    pub fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }
}

impl std::ops::BitOr for GpuTextureUsage {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
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
}

/// Buffer usage flags.
#[derive(Clone, Copy, Debug)]
pub struct GpuBufferUsage(u32);

impl GpuBufferUsage {
    pub const UNIFORM: Self = Self(1 << 0);
    pub const STORAGE: Self = Self(1 << 1);
    pub const COPY_DST: Self = Self(1 << 2);
    pub const VERTEX: Self = Self(1 << 3);
    pub const INDEX: Self = Self(1 << 4);
}

impl std::ops::BitOr for GpuBufferUsage {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self { Self(self.0 | rhs.0) }
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
        }
    }
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
    Bytes {
        binding: u32,
        data: &'a [u8],
    },
}
