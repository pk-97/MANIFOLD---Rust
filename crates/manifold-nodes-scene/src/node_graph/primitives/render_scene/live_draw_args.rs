//! Indirect draw arguments for scene objects, one aligned eight-word block
//! per object, written on the GPU each frame: from the mesh's live extent
//! (`scene::live_extent`), so every raster pass draws only the
//! live triangles, and from the instance array, so an instanced draw stops at
//! its last instance that is not an all-zero hole (`live_instances.wgsl`).
//! A camera pass reads a second block per culled draw: the live block with
//! only the copies that can reach the view counted (`cull_instances.wgsl`),
//! or one such block per detail level for a mesh that has them.

use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBinding, GpuBuffer, GpuComputePipeline, GpuDevice, GpuEncoder};

use manifold_node_engine::scene::live_extent::LiveExtent;
use manifold_node_engine::scene::mesh_bounds::MeshBounds;

/// Bytes of one object's draw arguments.
pub(super) const ARGS_BYTES: u64 = 32;

/// The most trim workgroups one dispatch takes; each strides past the grid.
const TRIM_GROUPS: u32 = 1024;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LiveArgs {
    word: u32,
    per_item: u32,
    capacity: u32,
    instances: u32,
    slot: u32,
    _pad: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct TrimArgs {
    slot: u32,
    count: u32,
    vertices: u32,
    _pad: u32,
}

/// The scene shader's instance helpers, then the cull kernel that places a
/// copy with them.
const CULL_SHADER: &str = concat!(
    include_str!("../shaders/instance_transform.wgsl"),
    include_str!("../shaders/cull_instances.wgsl"),
);

/// Fewest vertices a draw's copies must add up to for a cull pass to pay: a
/// lighter draw is cheaper to draw whole. About a thousand cubes; one heavy
/// scan repeated a few dozen times (a Scene Loop corridor) passes easily.
pub(super) const CULL_MIN_VERTICES: u64 = 32 * 1024;

/// `CullArgs` in `cull_instances.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CullArgs {
    clip: [[f32; 4]; 4],
    model: [[f32; 4]; 4],
    inner: [[f32; 4]; 4],
    box_min: [f32; 4],
    box_max: [f32; 4],
    count: u32,
    live_block: u32,
    view_block: u32,
    has_prev: u32,
    viewport: [f32; 2],
    levels: u32,
    _pad: u32,
}

/// One draw's cull: the matrices its uniforms carry, the mesh box, the copies
/// to test and where the survivors go.
pub(super) struct CullDraw<'a> {
    pub clip: &'a [[f32; 4]; 4],
    pub model: &'a [[f32; 4]; 4],
    pub inner: &'a [[f32; 4]; 4],
    pub bounds: &'a MeshBounds,
    pub count: u32,
    /// The draw's live block, copied into `view_block` with no instances.
    pub live_block: u32,
    pub view_block: u32,
    pub instances: &'a GpuBuffer,
    pub out_instances: &'a GpuBuffer,
    /// Last frame's records and the buffer their survivors go to, when the
    /// velocity pass reads them.
    pub prev: Option<(&'a GpuBuffer, &'a GpuBuffer)>,
    /// The mesh's detail levels, which take the [`view_blocks`] after
    /// `view_block`, and the view's size in pixels that picks among them.
    pub lods: Option<&'a GpuBuffer>,
    pub viewport: [u32; 2],
}

/// Arguments blocks one view of a culled draw takes: the full mesh's, and
/// one per detail level when the mesh has them.
pub(super) fn view_blocks(lods: bool) -> u64 {
    if lods {
        1 + u64::from(super::super::mesh_lods::LOD_LEVELS)
    } else {
        1
    }
}

#[derive(Default)]
pub(super) struct LiveDrawArgs {
    pipeline: Option<GpuComputePipeline>,
    fixed: Option<GpuComputePipeline>,
    trim: Option<GpuComputePipeline>,
    view_args: Option<GpuComputePipeline>,
    cull: Option<GpuComputePipeline>,
    count_levels: Option<GpuComputePipeline>,
    level_bases: Option<GpuComputePipeline>,
    place_levels: Option<GpuComputePipeline>,
    buffer: Option<GpuBuffer>,
}

impl LiveDrawArgs {
    /// The arguments buffer, holding at least `blocks` blocks: one per live
    /// draw, then one per culled draw.
    pub(super) fn prepare(&mut self, device: &GpuDevice, blocks: usize) -> GpuBuffer {
        if self.pipeline.is_none() {
            self.pipeline = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_draw_args.wgsl"),
                "write_draw_args",
                "node.render_scene live draw args",
            ));
            self.fixed = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_instances.wgsl"),
                "fixed_args",
                "node.render_scene fixed draw args",
            ));
            self.trim = Some(device.create_compute_pipeline(
                include_str!("../shaders/live_instances.wgsl"),
                "trim_instances",
                "node.render_scene live instances",
            ));
            self.view_args = Some(device.create_compute_pipeline(
                CULL_SHADER,
                "view_args",
                "node.render_scene view draw args",
            ));
            self.cull = Some(device.create_compute_pipeline(
                CULL_SHADER,
                "cull",
                "node.render_scene cull instances",
            ));
            self.count_levels = Some(device.create_compute_pipeline(
                CULL_SHADER,
                "count_levels",
                "node.render_scene count copy levels",
            ));
            self.level_bases = Some(device.create_compute_pipeline(
                CULL_SHADER,
                "level_bases",
                "node.render_scene copy level bases",
            ));
            self.place_levels = Some(device.create_compute_pipeline(
                CULL_SHADER,
                "place_levels",
                "node.render_scene place copy levels",
            ));
        }
        let bytes = blocks.max(1) as u64 * ARGS_BYTES;
        if self
            .buffer
            .as_ref()
            .is_none_or(|buffer| buffer.size < bytes)
        {
            self.buffer = Some(device.create_buffer(bytes, BufferUse::Gpu));
        }
        self.buffer.clone().expect("arguments buffer allocated")
    }

    /// Encode block `slot`: the extent's live vertices as whole triangles,
    /// clamped to the vertex or index buffer capacity, drawn `instances` times.
    pub(super) fn write(
        &self,
        encoder: &mut GpuEncoder,
        slot: usize,
        extent: &LiveExtent,
        capacity: u32,
        instances: u32,
    ) {
        let params = LiveArgs {
            word: (extent.offset / 4) as u32,
            per_item: extent.per_item,
            capacity,
            instances,
            slot: slot as u32,
            _pad: [0; 3],
        };
        encoder.dispatch_compute(
            self.pipeline.as_ref().expect("prepare before write"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: &extent.counts,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.buffer.as_ref().expect("prepare before write"),
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene live draw args",
        );
    }

    /// Encode block `slot` for a mesh without a live extent: `vertices`
    /// (indices when indexed) and zero instances, for [`Self::trim`].
    pub(super) fn write_fixed(&self, encoder: &mut GpuEncoder, slot: usize, vertices: u32) {
        let params = TrimArgs {
            slot: slot as u32,
            count: 0,
            vertices,
            _pad: 0,
        };
        encoder.dispatch_compute(
            self.fixed.as_ref().expect("prepare before write"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.buffer.as_ref().expect("prepare before write"),
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene fixed draw args",
        );
    }

    /// Raise block `slot`'s instance word, written as zero and behind a
    /// buffer barrier, to one past the last of the first `count` instances
    /// that is not all zero.
    pub(super) fn trim(
        &self,
        encoder: &mut GpuEncoder,
        slot: usize,
        instances: &GpuBuffer,
        count: u32,
    ) {
        let params = TrimArgs {
            slot: slot as u32,
            count,
            vertices: 0,
            _pad: 0,
        };
        encoder.dispatch_compute(
            self.trim.as_ref().expect("prepare before trim"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: instances,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: self.buffer.as_ref().expect("prepare before trim"),
                    offset: 0,
                },
            ],
            [count.div_ceil(256).clamp(1, TRIM_GROUPS), 1, 1],
            "node.render_scene live instances",
        );
    }

    /// Fill view block `draw.view_block` from live block `draw.live_block`
    /// (written and trimmed behind a buffer barrier) and move the copies of
    /// the first `draw.count` whose box can reach the clip volume into
    /// `draw.out_instances`, counting them into the view block's instance
    /// word. Survivors keep their previous records alongside when `draw.prev`
    /// is set. With detail levels, each copy is counted into its level's
    /// block, and the survivors are laid out level by level.
    pub(super) fn cull(&self, encoder: &mut GpuEncoder, draw: &CullDraw<'_>) {
        let buffer = self.buffer.as_ref().expect("prepare before cull");
        let params = CullArgs {
            clip: *draw.clip,
            model: *draw.model,
            inner: *draw.inner,
            box_min: [
                draw.bounds.min[0],
                draw.bounds.min[1],
                draw.bounds.min[2],
                0.0,
            ],
            box_max: [
                draw.bounds.max[0],
                draw.bounds.max[1],
                draw.bounds.max[2],
                0.0,
            ],
            count: draw.count,
            live_block: draw.live_block,
            view_block: draw.view_block,
            has_prev: u32::from(draw.prev.is_some()),
            viewport: [draw.viewport[0] as f32, draw.viewport[1] as f32],
            levels: (view_blocks(draw.lods.is_some()) - 1) as u32,
            _pad: 0,
        };
        // Without detail levels the binding takes a stand-in the kernels
        // never index past word 0 of, read as no levels.
        let lods = draw.lods.unwrap_or(draw.instances);
        encoder.dispatch_compute(
            self.view_args.as_ref().expect("prepare before cull"),
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&params),
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 6,
                    buffer: lods,
                    offset: 0,
                },
            ],
            [1, 1, 1],
            "node.render_scene view draw args",
        );
        encoder.compute_memory_barrier_buffers();
        // Without previous records the two bindings take stand-ins the
        // kernel never touches.
        let (prev, out_prev) = draw.prev.unwrap_or((draw.instances, draw.out_instances));
        let bindings = [
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&params),
            },
            GpuBinding::Buffer {
                binding: 1,
                buffer: draw.instances,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 2,
                buffer,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 3,
                buffer: draw.out_instances,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 4,
                buffer: prev,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 5,
                buffer: out_prev,
                offset: 0,
            },
            GpuBinding::Buffer {
                binding: 6,
                buffer: lods,
                offset: 0,
            },
        ];
        let groups = [draw.count.div_ceil(256).clamp(1, TRIM_GROUPS), 1, 1];
        if draw.lods.is_none() {
            encoder.dispatch_compute(
                self.cull.as_ref().expect("prepare before cull"),
                &bindings,
                groups,
                "node.render_scene cull instances",
            );
            return;
        }
        encoder.dispatch_compute(
            self.count_levels.as_ref().expect("prepare before cull"),
            &bindings,
            groups,
            "node.render_scene count copy levels",
        );
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(
            self.level_bases.as_ref().expect("prepare before cull"),
            &bindings,
            [1, 1, 1],
            "node.render_scene copy level bases",
        );
        encoder.compute_memory_barrier_buffers();
        encoder.dispatch_compute(
            self.place_levels.as_ref().expect("prepare before cull"),
            &bindings,
            groups,
            "node.render_scene place copy levels",
        );
    }
}

#[cfg(all(test, feature = "gpu-proofs"))]
mod tests {
    use super::*;

    /// Whole live triangles, clamped to the vertex buffer's capacity, in the
    /// object's own aligned block.
    #[test]
    fn live_draw_args_are_whole_live_triangles_within_capacity() {
        let device = manifold_gpu::testkit::test_device();
        let mut writer = LiveDrawArgs::default();
        for (triangles, expected) in [(4u32, 12u32), (7, 18)] {
            let counts = device.create_buffer(8, BufferUse::Upload);
            // SAFETY: fresh shared buffer, no GPU work in flight.
            unsafe { counts.write(0, bytemuck::cast_slice(&[0u32, triangles])) };
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("live draw args test");
            let extent = LiveExtent {
                counts,
                offset: 4,
                per_item: 3,
                bound: 18,
            };
            writer.write(&mut encoder, 1, &extent, 18, 5);
            encoder.commit_and_wait().expect("GPU work completes");
            let bytes = manifold_gpu::testkit::read_bytes(&args, 64);
            let words: Vec<u32> = bytes
                .chunks_exact(4)
                .map(bytemuck::pod_read_unaligned)
                .collect();
            assert_eq!(
                &words[8..13],
                &[expected, 5, 0, 0, 0],
                "{triangles} triangles in an 18-vertex buffer"
            );
        }
    }

    /// One past the last instance that is not all zero, within the count;
    /// holes before it still count. Over 1024 groups' worth, the stride
    /// reaches the end.
    #[test]
    fn trimmed_instances_end_after_the_last_non_hole() {
        let device = manifold_gpu::testkit::test_device();
        let mut writer = LiveDrawArgs::default();
        let live = [1.0f32, 2.0, 3.0, 0.5, 0.0, 0.0, 0.0, 0.0];
        let hole = [0.0f32; 8];
        let mut far = vec![hole; 300_000];
        far[299_998] = live;
        let mut sign_only = hole;
        sign_only[7] = -1.0;
        let cases: [(Vec<[f32; 8]>, u32, u32); 6] = [
            (vec![live, hole, live, hole, hole], 5, 3),
            (vec![hole, hole, hole], 3, 0),
            (vec![live, hole, live], 2, 1),
            (vec![hole, sign_only], 2, 2),
            (vec![live; 1000], 1000, 1000),
            (far, 300_000, 299_999),
        ];
        for (instances, count, expected) in cases {
            let buffer = device.create_buffer((instances.len() * 32) as u64, BufferUse::Upload);
            // SAFETY: fresh shared buffer, no GPU work in flight.
            unsafe { buffer.write(0, bytemuck::cast_slice(&instances)) };
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("live instances test");
            writer.write_fixed(&mut encoder, 1, 36);
            encoder.compute_memory_barrier_buffers();
            writer.trim(&mut encoder, 1, &buffer, count);
            encoder.commit_and_wait().expect("GPU work completes");
            let bytes = manifold_gpu::testkit::read_bytes(&args, 64);
            let words: Vec<u32> = bytes
                .chunks_exact(4)
                .map(bytemuck::pod_read_unaligned)
                .collect();
            assert_eq!(
                &words[8..13],
                &[36, expected, 0, 0, 0],
                "{} instances, count {count}",
                instances.len()
            );
        }
    }

    /// A camera at the origin looking down -Z, 90 degrees across, Metal's
    /// clip volume (`0 <= z <= w`).
    fn camera_clip() -> [[f32; 4]; 4] {
        let (near, far) = (0.1f32, 100.0f32);
        let a = far / (near - far);
        let b = far * near / (near - far);
        [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, a, -1.0],
            [0.0, 0.0, b, 0.0],
        ]
    }

    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// Five unit boxes: centred in view, far left, behind the camera, a hole,
    /// and one straddling the left edge. Only the centred and straddling
    /// copies survive, the live block is untouched, and with previous
    /// records each survivor carries its own.
    #[test]
    fn cull_keeps_copies_that_reach_the_view() {
        let device = manifold_gpu::testkit::test_device();
        let mut writer = LiveDrawArgs::default();
        let copy = |x: f32, z: f32| [x, 0.0, z, 1.0, 0.0, 0.0, 0.0, 0.0];
        let instances = [
            copy(0.0, -5.0),
            copy(-50.0, -5.0),
            copy(0.0, 5.0),
            [0.0f32; 8],
            copy(-5.0, -5.0),
        ];
        let prev: Vec<[f32; 8]> = (0..5)
            .map(|i| {
                [
                    i as f32 + 0.25,
                    7.0,
                    8.0,
                    1.0,
                    0.0,
                    0.0,
                    0.0,
                    100.0 + i as f32,
                ]
            })
            .collect();
        let bounds = MeshBounds {
            min: [-0.5; 3],
            max: [0.5; 3],
        };
        let clip = camera_clip();
        for with_prev in [false, true] {
            let source = device.create_buffer(5 * 32, BufferUse::Upload);
            let out = device.create_buffer(5 * 32, BufferUse::Upload);
            let prev_source = device.create_buffer(5 * 32, BufferUse::Upload);
            let out_prev = device.create_buffer(5 * 32, BufferUse::Upload);
            // SAFETY: fresh shared buffers, no GPU work in flight.
            unsafe {
                source.write(0, bytemuck::cast_slice(&instances));
                prev_source.write(0, bytemuck::cast_slice(&prev));
                out.write(0, &[0u8; 5 * 32]);
                out_prev.write(0, &[0u8; 5 * 32]);
            }
            let args = writer.prepare(&device, 2);
            let mut encoder = device.create_encoder("cull instances test");
            writer.write_fixed(&mut encoder, 0, 36);
            encoder.compute_memory_barrier_buffers();
            writer.trim(&mut encoder, 0, &source, 5);
            encoder.compute_memory_barrier_buffers();
            writer.cull(
                &mut encoder,
                &CullDraw {
                    clip: &clip,
                    model: &IDENTITY,
                    inner: &IDENTITY,
                    bounds: &bounds,
                    count: 5,
                    live_block: 0,
                    view_block: 1,
                    instances: &source,
                    out_instances: &out,
                    prev: with_prev.then_some((&prev_source, &out_prev)),
                },
            );
            encoder.commit_and_wait().expect("GPU work completes");
            let words: Vec<u32> = manifold_gpu::testkit::read_bytes(&args, 64)
                .chunks_exact(4)
                .map(bytemuck::pod_read_unaligned)
                .collect();
            assert_eq!(&words[0..5], &[36, 5, 0, 0, 0], "live block untouched");
            assert_eq!(
                &words[8..13],
                &[36, 2, 0, 0, 0],
                "view block counts survivors"
            );
            let kept = unsafe {
                std::slice::from_raw_parts(out.mapped_ptr().expect("shared") as *const [f32; 8], 2)
            };
            let kept_prev = unsafe {
                std::slice::from_raw_parts(
                    out_prev.mapped_ptr().expect("shared") as *const [f32; 8],
                    2,
                )
            };
            let mut seen = Vec::new();
            for (k, record) in kept.iter().enumerate() {
                let index = if *record == instances[0] {
                    0
                } else if *record == instances[4] {
                    4
                } else {
                    panic!("unexpected survivor {record:?}");
                };
                seen.push(index);
                if with_prev {
                    assert_eq!(kept_prev[k], prev[index], "previous record of copy {index}");
                } else {
                    assert_eq!(kept_prev[k], [0.0; 8], "previous records stay untouched");
                }
            }
            seen.sort_unstable();
            assert_eq!(seen, [0, 4]);
        }
    }
}
