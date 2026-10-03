//! Publication-only stable radix sort. Section 2.5 audit: spatial binning
//! cannot order arbitrary u32 birth IDs; reuse its barriered PrefixScan.
//! This multi-pass helper belongs to liquid_frame's cross-frame boundary.
use super::liquid_stats::with_stats_layout;
use super::prefix_scan::{PrefixScan, ScanLabels, storage_words};
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

const SHADER: &str = include_str!("shaders/particle_publication.wgsl");
pub(crate) fn scratch_bytes(slots: u32) -> u64 {
    u64::from(slots.max(1)) * 32 + storage_words(slots.max(1) as usize) as u64 * 4
}
#[derive(Default)]
pub struct ParticlePublication {
    pipelines: Option<[GpuComputePipeline; 4]>,
    scratch: Option<GpuBuffer>,
    scan: PrefixScan,
}
pub(crate) struct Publication<'a> {
    pub source: &'a GpuBuffer,
    pub target: &'a GpuBuffer,
    pub identity: &'a GpuBuffer,
    pub stats: &'a GpuBuffer,
    pub metadata: &'a GpuBuffer,
    pub count: u32,
}
impl ParticlePublication {
    pub(crate) fn prepare(&mut self, device: &GpuDevice) {
        if self.pipelines.is_none() {
            let shader = with_stats_layout(SHADER);
            self.pipelines = Some(["initialize", "flags", "scatter", "publish_metadata"].map(
                |entry| device.create_compute_pipeline(&shader, entry, "particle_publication"),
            ));
        }
        self.scan.prepare(device);
    }
    pub(crate) fn encode(
        &mut self,
        device: &GpuDevice,
        enc: &mut GpuEncoder,
        job: Publication<'_>,
    ) -> Result<(), String> {
        let slots = (job.target.size / 32) as u32;
        if slots == 0
            || u64::from(job.count) * 32 > job.source.size
            || job.count > slots
            || job.identity.size < 16
            || job.metadata.size < 16
            || job.stats.size < u64::from(super::liquid_stats::LIQUID_STATS_WORDS) * 4
        {
            return Err("particle publication buffers do not cover dispatch extent".into());
        }
        if self
            .scratch
            .as_ref()
            .is_none_or(|s| s.size < job.target.size)
        {
            crate::node_graph::scene_modifier_expand::admit_candidate_bytes(
                device.modifier_memory_snapshot(),
                scratch_bytes(slots),
            )
            .map_err(|error| error.to_string())?;
            self.scratch = Some(device.try_create_buffer_shared(job.target.size)?);
        }
        let scan = self.scan.buffer(device, slots as usize)?.clone();
        let scratch = self.scratch.as_ref().expect("publication reserved");
        let pipes = self.pipelines.as_ref().expect("publication prepared");
        let groups = [slots.div_ceil(256), 1, 1];
        let mut params = [job.count, slots, 0, 0];
        enc.dispatch_compute(
            &pipes[0],
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&params),
                },
                binding(1, job.source),
                binding(2, scratch),
            ],
            groups,
            "particle_publication.initialize",
        );
        enc.compute_memory_barrier_buffers();
        // The final validity bit compacts even a live u32::MAX identity.
        for bit in 0..=32 {
            params[2] = bit;
            let (source, target) = if bit % 2 == 0 {
                (scratch, job.target)
            } else {
                (job.target, scratch)
            };
            enc.dispatch_compute(
                &pipes[1],
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    binding(1, source),
                    binding(3, &scan),
                ],
                groups,
                "particle_publication.flags",
            );
            enc.compute_memory_barrier_buffers();
            self.scan
                .encode_labelled(enc, slots as usize, ScanLabels::DEFAULT);
            enc.dispatch_compute(
                &pipes[2],
                &[
                    GpuBinding::Bytes {
                        binding: 0,
                        data: bytemuck::cast_slice(&params),
                    },
                    binding(1, source),
                    binding(2, target),
                    binding(3, &scan),
                ],
                groups,
                "particle_publication.scatter",
            );
            enc.compute_memory_barrier_buffers();
        }
        enc.dispatch_compute(
            &pipes[3],
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::cast_slice(&params),
                },
                binding(3, &scan),
                binding(4, job.identity),
                binding(5, job.stats),
                binding(6, job.metadata),
            ],
            [1, 1, 1],
            "particle_publication.metadata",
        );
        enc.compute_memory_barrier_buffers();
        Ok(())
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
    fn particle_publication_shader_validates() {
        let shader = super::with_stats_layout(super::SHADER);
        let module = naga::front::wgsl::parse_str(&shader).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .unwrap();
    }
    #[test]
    fn particle_publication_dispatch_extents_cover_every_buffer() {
        for slots in [1u32, 2, 4, 8, 257, 4096, 32768, 1 << 20] {
            let mut flags = vec![0u32; slots as usize];
            for (i, flag) in flags.iter_mut().enumerate() {
                *flag = u32::from(i % 3 == 0);
            }
            let mut sum = 0;
            let scan: Vec<_> = flags
                .iter()
                .map(|f| {
                    sum += f;
                    sum
                })
                .collect();
            let mut seen = vec![false; slots as usize];
            for i in 0..slots as usize {
                let destination = if flags[i] == 1 {
                    scan[i] - 1
                } else {
                    sum + i as u32 - scan[i]
                };
                assert!(destination < slots);
                assert!(!seen[destination as usize]);
                seen[destination as usize] = true;
            }
            assert!(seen.into_iter().all(|v| v));
            assert!(super::scratch_bytes(slots) >= u64::from(slots) * 36);
        }
    }
}
