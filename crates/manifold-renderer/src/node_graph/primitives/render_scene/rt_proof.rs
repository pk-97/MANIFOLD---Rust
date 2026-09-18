//! Opt-in observation of the actual scene consumed by RT, without re-evaluation.
use manifold_gpu::{GpuBuffer, GpuTexture};

pub struct RtProbeObject {
    pub vertices: GpuBuffer,
    pub indices: Option<GpuBuffer>,
    pub vertex_stride: u32,
    pub vertex_offset: u32,
    pub triangle_count: u32,
    pub transform: [[f32; 4]; 4],
    pub instances: Option<GpuBuffer>,
    pub instance_slots: u32,
    pub weights: Option<GpuBuffer>,
    pub gain: f32,
}

pub struct RtProbeScene {
    pub objects: Vec<RtProbeObject>,
    pub(super) textures: Vec<GpuTexture>,
}

impl RtProbeScene {
    pub(super) fn capture(objects: &[manifold_gpu::raytrace::RtObjectGeometry], textures: &[&GpuTexture]) -> Self {
        Self {
            objects: objects.iter().map(|object| RtProbeObject {
                vertices: object.vertex_buffer.clone(),
                indices: object.index_buffer.cloned(),
                vertex_stride: object.vertex_stride,
                vertex_offset: object.vertex_offset,
                triangle_count: object.triangle_count,
                transform: object.transform,
                instances: object.instances_buffer.cloned(),
                instance_slots: object.instance_slots,
                weights: object.appearance_weights.cloned(),
                gain: object.appearance_gain,
            }).collect(),
            textures: textures.iter().map(|texture| (*texture).clone()).collect(),
        }
    }
}
