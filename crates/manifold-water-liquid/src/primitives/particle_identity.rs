//! GPU-owned birth allocation and the live count beside liquid_state.
//! Stateful single-writer operations; the emission scan supplies accepted
//! rank, never a working slot.
//!
//! The identity buffer is [`IDENTITY_BYTES`] of u32 words:
//! - 0: next id (0 means the epoch's ids ran out).
//! - 1: identity epoch.
//! - 2: reserved base of the last birth stage.
//! - 3: full-reset request.
//! - 4-6: `[groups, 1, 1]`, a dispatch triple at 256 threads a group, at
//!   least one group so a pass that writes a total still runs on empty water.
//! - 7: `n`, the slots in use. Slots `[0, n)` may hold particles; a dead one
//!   there reads radius 0 and the next sort drops it. A pass reads no slot at
//!   or past `n`.
//!
//! [`COUNT_OFFSET`] is the byte offset of words 4-7, which a step record
//! `[groups, 1, 1, n]` repeats at offset 0. `seed` sets `n` to the fill's
//! count, `settle` to a sort's live end, `reserve` to the live end plus the
//! births it grants.
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

pub const IDENTITY_BYTES: u64 = 32;
/// Byte offset of the dispatch triple and `n` in the identity buffer.
pub const COUNT_OFFSET: u64 = 16;
/// Bytes of a step record `[groups, 1, 1, n]`.
const RECORD_BYTES: u64 = 16;
const SHADER: &str = include_str!("shaders/particle_identity.wgsl");
#[derive(Default)]
pub struct ParticleIdentity {
    seed: Option<GpuComputePipeline>,
    settle: Option<GpuComputePipeline>,
    reserve: Option<GpuComputePipeline>,
}
manifold_core::testkit_visible! {
pub struct BirthReservation<'a> {
    pub particles: &'a GpuBuffer,
    pub identity: &'a GpuBuffer,
    pub ranges: &'a GpuBuffer,
    pub scan: &'a GpuBuffer,
    pub plan: &'a GpuBuffer,
    /// The step's own record, written beside the identity's.
    pub step_record: Option<&'a GpuBuffer>,
    pub params: [u32; 4],
}
}
impl ParticleIdentity {
    manifold_core::testkit_visible! {
        pub fn prepare(&mut self, device: &GpuDevice) {
            if self.seed.is_none() {
                self.seed =
                    Some(device.create_compute_pipeline(SHADER, "seed", "particle_identity.seed"));
                self.settle = Some(device.create_compute_pipeline(
                    SHADER,
                    "settle",
                    "particle_identity.settle",
                ));
                self.reserve = Some(device.create_compute_pipeline(
                    SHADER,
                    "reserve",
                    "particle_identity.reserve",
                ));
            }
        }
    }
    manifold_core::testkit_visible! {
        pub fn seed(
            &self,
            enc: &mut GpuEncoder,
            particles: &GpuBuffer,
            identity: &GpuBuffer,
            count: u32,
            epoch: u32,
        ) {
            assert!(u64::from(count) * 32 <= particles.size);
            assert!(identity.size >= IDENTITY_BYTES);
            let params = [count, epoch, 0, 0, 0, 0, 0, 0];
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
    }
    manifold_core::testkit_visible! {
        /// After a sort over `keys` ranges: `n` is the live end, the last
        /// key's range end. With `step_record` the step's record gets it too.
        /// `plan` is the clock plan; an inactive slot writes nothing.
        pub fn settle(
            &self,
            enc: &mut GpuEncoder,
            ranges: &GpuBuffer,
            keys: u32,
            identity: &GpuBuffer,
            plan: &GpuBuffer,
            step_record: Option<&GpuBuffer>,
        ) {
            assert!(keys > 0 && u64::from(keys) * 8 <= ranges.size);
            assert!(identity.size >= IDENTITY_BYTES && plan.size >= 12 * 4);
            assert!(step_record.is_none_or(|record| record.size >= RECORD_BYTES));
            let params = [0, keys, 0, 0, u32::from(step_record.is_some()), 0, 0, 0];
            enc.dispatch_compute(
                self.settle.as_ref().expect("identity prepared"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    binding(2, identity),
                    binding(3, ranges),
                    binding(5, plan),
                    binding(6, step_record.unwrap_or(identity)),
                ],
                [1, 1, 1],
                "particle_identity.settle",
            );
            enc.compute_memory_barrier_buffers();
        }
    }
    manifold_core::testkit_visible! {
        pub fn reserve(&self, enc: &mut GpuEncoder, job: BirthReservation<'_>) {
            // Prove every single-writer access before dispatch, including the rare
            // rollover loop. The shader separately bounds the GPU live prefix.
            assert!(u64::from(job.params[0]) * 32 <= job.particles.size);
            assert!(job.params[1] > 0 && u64::from(job.params[1]) * 8 <= job.ranges.size);
            assert!(job.params[2] > 0 && u64::from(job.params[2]) * 4 <= job.scan.size);
            assert!(job.identity.size >= IDENTITY_BYTES && job.plan.size >= 12 * 4);
            assert!(
                job.step_record
                    .is_none_or(|record| record.size >= RECORD_BYTES)
            );
            let params = [
                job.params[0],
                job.params[1],
                job.params[2],
                job.params[3],
                u32::from(job.step_record.is_some()),
                0,
                0,
                0,
            ];
            enc.dispatch_compute(
                self.reserve.as_ref().expect("identity prepared"),
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    binding(1, job.particles),
                    binding(2, job.identity),
                    binding(3, job.ranges),
                    binding(4, job.scan),
                    binding(5, job.plan),
                    binding(6, job.step_record.unwrap_or(job.identity)),
                ],
                [1, 1, 1],
                "particle_identity.reserve",
            );
            enc.compute_memory_barrier_buffers();
        }
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

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
