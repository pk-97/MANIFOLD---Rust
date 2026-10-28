//! Upscaling and denoising on Vulkan are P7 (DLSS, FSR or NRD, design
//! section 11). Until then none of these can be built: `new` returns
//! `None` and `GpuDevice::supports_upscaler`/`supports_denoiser` are false.

// The never-built types' methods take parameters they cannot reach.
#![allow(unused_variables)]

use std::convert::Infallible;

use super::{GpuDevice, GpuEncoder, GpuTexture};

pub struct GpuSpatialUpscaler {
    never: Infallible,
}

impl GpuSpatialUpscaler {
    pub fn new(device: &GpuDevice, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Option<Self> {
        None
    }

    pub fn is_available(device: &GpuDevice) -> bool {
        false
    }

    pub fn encode(&self, enc: &mut GpuEncoder, source: &GpuTexture, output: &GpuTexture) {
        match self.never {}
    }
}

pub struct GpuTemporalUpscaler {
    never: Infallible,
}

impl GpuTemporalUpscaler {
    pub fn new(device: &GpuDevice, src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> Option<Self> {
        None
    }

    pub fn is_available(device: &GpuDevice) -> bool {
        false
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        output: &GpuTexture,
        jitter_offset_x: f32,
        jitter_offset_y: f32,
        reset: bool,
    ) -> bool {
        match self.never {}
    }

    pub fn resize(
        &mut self,
        device: &GpuDevice,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> bool {
        match self.never {}
    }
}

pub struct GpuTemporalDenoiser {
    never: Infallible,
}

impl GpuTemporalDenoiser {
    pub fn new(
        device: &GpuDevice,
        input_width: u32,
        input_height: u32,
        output_width: u32,
        output_height: u32,
        depth_reversed: bool,
    ) -> Option<Self> {
        None
    }

    pub fn set_jitter(&self, x: f32, y: f32) {
        match self.never {}
    }

    #[allow(clippy::too_many_arguments)]
    pub fn encode(
        &self,
        enc: &mut GpuEncoder,
        color: &GpuTexture,
        depth: &GpuTexture,
        motion: &GpuTexture,
        normal: &GpuTexture,
        roughness: &GpuTexture,
        diffuse_albedo: &GpuTexture,
        specular_albedo: &GpuTexture,
        specular_hit_distance: &GpuTexture,
        output: &GpuTexture,
        reset: bool,
        reactive_mask: Option<&GpuTexture>,
    ) -> bool {
        match self.never {}
    }
}
