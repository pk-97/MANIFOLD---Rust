//! Metal format conversion helpers.
//!
//! Translates manifold-gpu abstract types to native Metal enums.

use crate::types::*;

pub(crate) fn to_mtl_pixel_format(format: GpuTextureFormat) -> metal::MTLPixelFormat {
    match format {
        GpuTextureFormat::Rgba16Float => metal::MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => metal::MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => metal::MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::R32Float => metal::MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => metal::MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => metal::MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => metal::MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => metal::MTLPixelFormat::R32Uint,
        GpuTextureFormat::Rgba8UnormSrgb => metal::MTLPixelFormat::RGBA8Unorm_sRGB,
        GpuTextureFormat::Bgra8Unorm => metal::MTLPixelFormat::BGRA8Unorm,
        GpuTextureFormat::R8Unorm => metal::MTLPixelFormat::R8Unorm,
        GpuTextureFormat::Depth32Float => metal::MTLPixelFormat::Depth32Float,
    }
}

pub(crate) fn to_mtl_texture_type(dim: GpuTextureDimension, _depth: u32) -> metal::MTLTextureType {
    match dim {
        GpuTextureDimension::D2 => metal::MTLTextureType::D2,
        GpuTextureDimension::D3 => metal::MTLTextureType::D3,
    }
}

pub(crate) fn to_mtl_storage_mode(mode: GpuStorageMode) -> metal::MTLStorageMode {
    match mode {
        GpuStorageMode::Private => metal::MTLStorageMode::Private,
        GpuStorageMode::Shared => metal::MTLStorageMode::Shared,
        GpuStorageMode::Managed => metal::MTLStorageMode::Managed,
        GpuStorageMode::Memoryless => metal::MTLStorageMode::Memoryless,
    }
}

pub(crate) fn to_mtl_texture_usage(usage: GpuTextureUsage) -> metal::MTLTextureUsage {
    let mut mtl = metal::MTLTextureUsage::Unknown;
    if usage.contains(GpuTextureUsage::SHADER_READ) {
        mtl |= metal::MTLTextureUsage::ShaderRead;
    }
    if usage.contains(GpuTextureUsage::SHADER_WRITE) {
        mtl |= metal::MTLTextureUsage::ShaderWrite;
    }
    if usage.contains(GpuTextureUsage::RENDER_TARGET) {
        mtl |= metal::MTLTextureUsage::RenderTarget;
    }
    mtl
}

pub(crate) fn to_mtl_filter(filter: GpuFilterMode) -> metal::MTLSamplerMinMagFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMinMagFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMinMagFilter::Linear,
    }
}

pub(crate) fn to_mtl_mip_filter(filter: GpuFilterMode) -> metal::MTLSamplerMipFilter {
    match filter {
        GpuFilterMode::Nearest => metal::MTLSamplerMipFilter::Nearest,
        GpuFilterMode::Linear => metal::MTLSamplerMipFilter::Linear,
    }
}

pub(crate) fn to_mtl_address(mode: GpuAddressMode) -> metal::MTLSamplerAddressMode {
    match mode {
        GpuAddressMode::ClampToEdge => metal::MTLSamplerAddressMode::ClampToEdge,
        GpuAddressMode::Repeat => metal::MTLSamplerAddressMode::Repeat,
        GpuAddressMode::MirrorRepeat => metal::MTLSamplerAddressMode::MirrorRepeat,
        GpuAddressMode::ClampToZero => metal::MTLSamplerAddressMode::ClampToZero,
    }
}

pub(crate) fn to_mtl_vertex_format(fmt: GpuVertexFormat) -> metal::MTLVertexFormat {
    match fmt {
        GpuVertexFormat::Float32 => metal::MTLVertexFormat::Float,
        GpuVertexFormat::Float32x2 => metal::MTLVertexFormat::Float2,
        GpuVertexFormat::Float32x3 => metal::MTLVertexFormat::Float3,
        GpuVertexFormat::Float32x4 => metal::MTLVertexFormat::Float4,
        GpuVertexFormat::Uint32 => metal::MTLVertexFormat::UInt,
        GpuVertexFormat::Uint8x4 => metal::MTLVertexFormat::UChar4,
    }
}

pub(crate) fn to_mtl_triangle_fill_mode(mode: GpuTriangleFillMode) -> metal::MTLTriangleFillMode {
    match mode {
        GpuTriangleFillMode::Fill => metal::MTLTriangleFillMode::Fill,
        GpuTriangleFillMode::Lines => metal::MTLTriangleFillMode::Lines,
    }
}

pub(crate) fn to_mtl_compare_function(func: GpuCompareFunction) -> metal::MTLCompareFunction {
    match func {
        GpuCompareFunction::Never => metal::MTLCompareFunction::Never,
        GpuCompareFunction::Less => metal::MTLCompareFunction::Less,
        GpuCompareFunction::Equal => metal::MTLCompareFunction::Equal,
        GpuCompareFunction::LessEqual => metal::MTLCompareFunction::LessEqual,
        GpuCompareFunction::Greater => metal::MTLCompareFunction::Greater,
        GpuCompareFunction::NotEqual => metal::MTLCompareFunction::NotEqual,
        GpuCompareFunction::GreaterEqual => metal::MTLCompareFunction::GreaterEqual,
        GpuCompareFunction::Always => metal::MTLCompareFunction::Always,
    }
}

pub(crate) fn to_mtl_blend_factor(factor: GpuBlendFactor) -> metal::MTLBlendFactor {
    match factor {
        GpuBlendFactor::Zero => metal::MTLBlendFactor::Zero,
        GpuBlendFactor::One => metal::MTLBlendFactor::One,
        GpuBlendFactor::SrcAlpha => metal::MTLBlendFactor::SourceAlpha,
        GpuBlendFactor::OneMinusSrcAlpha => metal::MTLBlendFactor::OneMinusSourceAlpha,
        GpuBlendFactor::DstAlpha => metal::MTLBlendFactor::DestinationAlpha,
        GpuBlendFactor::OneMinusDstAlpha => metal::MTLBlendFactor::OneMinusDestinationAlpha,
        GpuBlendFactor::SrcColor => metal::MTLBlendFactor::SourceColor,
        GpuBlendFactor::OneMinusSrcColor => metal::MTLBlendFactor::OneMinusSourceColor,
        GpuBlendFactor::DstColor => metal::MTLBlendFactor::DestinationColor,
        GpuBlendFactor::OneMinusDstColor => metal::MTLBlendFactor::OneMinusDestinationColor,
    }
}

pub(crate) fn to_mtl_blend_op(op: GpuBlendOp) -> metal::MTLBlendOperation {
    match op {
        GpuBlendOp::Add => metal::MTLBlendOperation::Add,
        GpuBlendOp::Subtract => metal::MTLBlendOperation::Subtract,
        GpuBlendOp::ReverseSubtract => metal::MTLBlendOperation::ReverseSubtract,
        GpuBlendOp::Min => metal::MTLBlendOperation::Min,
        GpuBlendOp::Max => metal::MTLBlendOperation::Max,
    }
}
