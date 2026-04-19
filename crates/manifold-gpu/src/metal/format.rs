//! Metal format conversion helpers.
//!
//! Translates manifold-gpu abstract types to native Metal enums.

use objc2_metal::{
    MTLBlendFactor, MTLBlendOperation, MTLCompareFunction, MTLPixelFormat, MTLPrimitiveType,
    MTLSamplerAddressMode, MTLSamplerMinMagFilter, MTLSamplerMipFilter, MTLStorageMode,
    MTLTextureType, MTLTextureUsage, MTLTriangleFillMode, MTLVertexFormat,
};

use crate::types::*;

pub(crate) fn to_mtl_pixel_format(format: GpuTextureFormat) -> MTLPixelFormat {
    match format {
        GpuTextureFormat::Rgba16Float => MTLPixelFormat::RGBA16Float,
        GpuTextureFormat::Rgba32Float => MTLPixelFormat::RGBA32Float,
        GpuTextureFormat::Rgba8Unorm => MTLPixelFormat::RGBA8Unorm,
        GpuTextureFormat::R32Float => MTLPixelFormat::R32Float,
        GpuTextureFormat::Rg32Float => MTLPixelFormat::RG32Float,
        GpuTextureFormat::R16Float => MTLPixelFormat::R16Float,
        GpuTextureFormat::Rg16Float => MTLPixelFormat::RG16Float,
        GpuTextureFormat::R32Uint => MTLPixelFormat::R32Uint,
        GpuTextureFormat::Rgba8UnormSrgb => MTLPixelFormat::RGBA8Unorm_sRGB,
        GpuTextureFormat::Bgra8Unorm => MTLPixelFormat::BGRA8Unorm,
        GpuTextureFormat::R8Unorm => MTLPixelFormat::R8Unorm,
        GpuTextureFormat::Depth32Float => MTLPixelFormat::Depth32Float,
    }
}

pub(crate) fn to_mtl_texture_type(dim: GpuTextureDimension, _depth: u32) -> MTLTextureType {
    match dim {
        GpuTextureDimension::D2 => MTLTextureType::Type2D,
        GpuTextureDimension::D3 => MTLTextureType::Type3D,
    }
}

pub(crate) fn to_mtl_storage_mode(mode: GpuStorageMode) -> MTLStorageMode {
    match mode {
        GpuStorageMode::Private => MTLStorageMode::Private,
        GpuStorageMode::Shared => MTLStorageMode::Shared,
        GpuStorageMode::Managed => MTLStorageMode::Managed,
        GpuStorageMode::Memoryless => MTLStorageMode::Memoryless,
    }
}

pub(crate) fn to_mtl_texture_usage(usage: GpuTextureUsage) -> MTLTextureUsage {
    let mut mtl = MTLTextureUsage::Unknown;
    if usage.contains(GpuTextureUsage::SHADER_READ) {
        mtl |= MTLTextureUsage::ShaderRead;
    }
    if usage.contains(GpuTextureUsage::SHADER_WRITE) {
        mtl |= MTLTextureUsage::ShaderWrite;
    }
    if usage.contains(GpuTextureUsage::RENDER_TARGET) {
        mtl |= MTLTextureUsage::RenderTarget;
    }
    mtl
}

pub(crate) fn to_mtl_filter(filter: GpuFilterMode) -> MTLSamplerMinMagFilter {
    match filter {
        GpuFilterMode::Nearest => MTLSamplerMinMagFilter::Nearest,
        GpuFilterMode::Linear => MTLSamplerMinMagFilter::Linear,
    }
}

pub(crate) fn to_mtl_mip_filter(filter: GpuFilterMode) -> MTLSamplerMipFilter {
    match filter {
        GpuFilterMode::Nearest => MTLSamplerMipFilter::Nearest,
        GpuFilterMode::Linear => MTLSamplerMipFilter::Linear,
    }
}

pub(crate) fn to_mtl_address(mode: GpuAddressMode) -> MTLSamplerAddressMode {
    match mode {
        GpuAddressMode::ClampToEdge => MTLSamplerAddressMode::ClampToEdge,
        GpuAddressMode::Repeat => MTLSamplerAddressMode::Repeat,
        GpuAddressMode::MirrorRepeat => MTLSamplerAddressMode::MirrorRepeat,
        GpuAddressMode::ClampToZero => MTLSamplerAddressMode::ClampToZero,
    }
}

pub(crate) fn to_mtl_vertex_format(fmt: GpuVertexFormat) -> MTLVertexFormat {
    match fmt {
        GpuVertexFormat::Float32 => MTLVertexFormat::Float,
        GpuVertexFormat::Float32x2 => MTLVertexFormat::Float2,
        GpuVertexFormat::Float32x3 => MTLVertexFormat::Float3,
        GpuVertexFormat::Float32x4 => MTLVertexFormat::Float4,
        GpuVertexFormat::Uint32 => MTLVertexFormat::UInt,
        GpuVertexFormat::Uint8x4 => MTLVertexFormat::UChar4,
    }
}

pub(crate) fn to_mtl_triangle_fill_mode(mode: GpuTriangleFillMode) -> MTLTriangleFillMode {
    match mode {
        GpuTriangleFillMode::Fill => MTLTriangleFillMode::Fill,
        GpuTriangleFillMode::Lines => MTLTriangleFillMode::Lines,
    }
}

pub(crate) fn to_mtl_primitive_type(prim: GpuPrimitiveType) -> MTLPrimitiveType {
    match prim {
        GpuPrimitiveType::Triangle => MTLPrimitiveType::Triangle,
        GpuPrimitiveType::Line => MTLPrimitiveType::Line,
    }
}

pub(crate) fn to_mtl_compare_function(func: GpuCompareFunction) -> MTLCompareFunction {
    match func {
        GpuCompareFunction::Never => MTLCompareFunction::Never,
        GpuCompareFunction::Less => MTLCompareFunction::Less,
        GpuCompareFunction::Equal => MTLCompareFunction::Equal,
        GpuCompareFunction::LessEqual => MTLCompareFunction::LessEqual,
        GpuCompareFunction::Greater => MTLCompareFunction::Greater,
        GpuCompareFunction::NotEqual => MTLCompareFunction::NotEqual,
        GpuCompareFunction::GreaterEqual => MTLCompareFunction::GreaterEqual,
        GpuCompareFunction::Always => MTLCompareFunction::Always,
    }
}

pub(crate) fn to_mtl_blend_factor(factor: GpuBlendFactor) -> MTLBlendFactor {
    match factor {
        GpuBlendFactor::Zero => MTLBlendFactor::Zero,
        GpuBlendFactor::One => MTLBlendFactor::One,
        GpuBlendFactor::SrcAlpha => MTLBlendFactor::SourceAlpha,
        GpuBlendFactor::OneMinusSrcAlpha => MTLBlendFactor::OneMinusSourceAlpha,
        GpuBlendFactor::DstAlpha => MTLBlendFactor::DestinationAlpha,
        GpuBlendFactor::OneMinusDstAlpha => MTLBlendFactor::OneMinusDestinationAlpha,
        GpuBlendFactor::SrcColor => MTLBlendFactor::SourceColor,
        GpuBlendFactor::OneMinusSrcColor => MTLBlendFactor::OneMinusSourceColor,
        GpuBlendFactor::DstColor => MTLBlendFactor::DestinationColor,
        GpuBlendFactor::OneMinusDstColor => MTLBlendFactor::OneMinusDestinationColor,
    }
}

pub(crate) fn to_mtl_blend_op(op: GpuBlendOp) -> MTLBlendOperation {
    match op {
        GpuBlendOp::Add => MTLBlendOperation::Add,
        GpuBlendOp::Subtract => MTLBlendOperation::Subtract,
        GpuBlendOp::ReverseSubtract => MTLBlendOperation::ReverseSubtract,
        GpuBlendOp::Min => MTLBlendOperation::Min,
        GpuBlendOp::Max => MTLBlendOperation::Max,
    }
}
