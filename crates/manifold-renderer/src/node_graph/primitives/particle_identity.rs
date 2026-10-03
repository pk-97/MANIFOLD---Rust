//! GPU-owned birth allocation beside liquid_state. Stateful single-writer
//! operations; the emission scan supplies accepted rank, never a working slot.
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

pub(crate) const IDENTITY_BYTES: u64 = 16;
const SHADER: &str = include_str!("shaders/particle_identity.wgsl");
#[derive(Default)]
pub struct ParticleIdentity {
    seed: Option<GpuComputePipeline>,
    reserve: Option<GpuComputePipeline>,
}
pub(crate) struct BirthReservation<'a> {
    pub particles: &'a GpuBuffer,
    pub identity: &'a GpuBuffer,
    pub ranges: &'a GpuBuffer,
    pub scan: &'a GpuBuffer,
    pub plan: &'a GpuBuffer,
    pub params: [u32; 4],
}
impl ParticleIdentity {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.seed.is_none() {
            self.seed =
                Some(device.create_compute_pipeline(SHADER, "seed", "particle_identity.seed"));
            self.reserve = Some(device.create_compute_pipeline(
                SHADER,
                "reserve",
                "particle_identity.reserve",
            ));
        }
    }
    pub(crate) fn seed(
        &self,
        enc: &mut GpuEncoder,
        particles: &GpuBuffer,
        identity: &GpuBuffer,
        count: u32,
        epoch: u32,
    ) {
        assert!(u64::from(count) * 32 <= particles.size);
        assert!(identity.size >= IDENTITY_BYTES);
        let params = [count, epoch, 0, 0];
        enc.dispatch_compute(
            self.seed.as_ref().expect("identity prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&params),
                },
                binding(1, particles),
                binding(2, identity),
            ],
            [1, 1, 1],
            "particle_identity.seed",
        );
        enc.compute_memory_barrier_buffers();
    }
    pub(crate) fn reserve(&self, enc: &mut GpuEncoder, job: BirthReservation<'_>) {
        // Prove every single-writer access before dispatch, including the rare
        // rollover loop. The shader separately bounds the GPU live prefix.
        assert!(u64::from(job.params[0]) * 32 <= job.particles.size);
        assert!(job.params[1] > 0 && u64::from(job.params[1]) * 8 <= job.ranges.size);
        assert!(job.params[2] > 0 && u64::from(job.params[2]) * 4 <= job.scan.size);
        assert!(job.identity.size >= IDENTITY_BYTES && job.plan.size >= 12 * 4);
        enc.dispatch_compute(
            self.reserve.as_ref().expect("identity prepared"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&job.params),
                },
                binding(1, job.particles),
                binding(2, job.identity),
                binding(3, job.ranges),
                binding(4, job.scan),
                binding(5, job.plan),
            ],
            [1, 1, 1],
            "particle_identity.reserve",
        );
        enc.compute_memory_barrier_buffers();
    }
}
fn binding(binding: u32, buffer: &GpuBuffer) -> GpuBinding<'_> {
    GpuBinding::Buffer {
        binding,
        buffer,
        offset: 0,
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn particle_identity_shader_validates() {
        let module = naga::front::wgsl::parse_str(super::SHADER).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
}
