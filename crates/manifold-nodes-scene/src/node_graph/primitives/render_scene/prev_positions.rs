//! Motion that is not the object's own transform: vertices that move inside
//! their mesh and copies that move inside their instance array.
//!
//! The velocity pass projects each vertex through the previous frame's object,
//! instance and camera transforms, so a vertex or a copy that moves on its own
//! has no motion of its own. Two histories fix that, kept per object and decided
//! by the one table in [`advance`]:
//!
//! - A mesh that deforms with the same triangles (its producer advances the
//!   positions token and holds the topology token) keeps last frame's vertex
//!   positions in a buffer: the velocity pass reads them in place of the
//!   current positions, and a compute pass after Pass A refreshes them.
//! - An instance array whose content changes while its buffer and count stay
//!   (a swarm, physics copies, a scatter that moves) keeps last frame's
//!   instance records in a buffer: the velocity pass reads each copy's
//!   previous transform from it, and a buffer copy after Pass A refreshes it.
//!
//! A mesh whose triangles are rebuilt every frame has no history to keep: its
//! producer may wire its own previous positions on the object instead (a
//! liquid's surface mesh, moved back along its velocity), and the velocity
//! pass reads those.
//!
//! Geometry that never changes never allocates a buffer, and none of this runs
//! for a draw that does not write velocity.

use super::*;
use manifold_gpu::BufferUse;

/// One previous-position record: `vec4<f32>` (xyz, w = 1).
pub(super) const RECORD_BYTES: u64 = 16;
/// One instance record, the `Instance` struct of `render_scene.wgsl`.
const INSTANCE_BYTES: u64 = std::mem::size_of::<InstanceTransform>() as u64;
/// Threads per workgroup in `pack_prev_positions.wgsl`.
const PACK_GROUP: u32 = 256;
const PACK_SHADER: &str = include_str!("../shaders/pack_prev_positions.wgsl");

/// `PackArgs` in `pack_prev_positions.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct PackArgs {
    count: u32,
    _pad: [u32; 3],
}

/// The vertex records a draw can read: an indexed draw reaches any record of
/// the buffer, a plain draw its first `draw_vertices` (its live bound).
fn read_extent(indexed: bool, draw_vertices: u32, records: u32) -> u32 {
    if indexed {
        records
    } else {
        draw_vertices.min(records)
    }
}

/// The vertex records a draw reads of its vertex buffer.
fn draw_extent(draw: &ObjectDraw<'_>) -> u32 {
    let records = (draw.vertices.size / std::mem::size_of::<MeshVertex>() as u64)
        .min(u64::from(u32::MAX)) as u32;
    read_extent(draw.indices.is_some(), draw.vertex_count, records)
}

/// What identifies the records a draw reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Tokens<Same, Version> {
    /// The source buffer, and how many of its records the draw reads.
    buffer: usize,
    extent: u32,
    /// What else must match for last frame's records to be these records'
    /// past: a mesh's triangles. An instance array has nothing past its buffer
    /// and count.
    same: Same,
    /// Changes when the records' values change.
    version: Version,
}

/// A mesh: its topology revision and its positions revision.
type VertexTokens = Tokens<u64, u64>;
/// An instance array: the content version its producer published.
type InstanceTokens = Tokens<(), ContentVersion>;

/// What the last frame that drew this object left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen<Same, Version> {
    tokens: Tokens<Same, Version>,
    /// Records the previous-record buffer holds; `None` until the geometry
    /// first moves.
    held: Option<u32>,
}

/// What this frame does for one draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Step {
    /// The velocity pass reads last frame's records from the buffer.
    motion: bool,
    /// A pass after Pass A copies this frame's records into it.
    copy: bool,
    /// The buffer is (re)allocated for `extent` records before the copy.
    allocate: bool,
}

const IDLE: Step = Step {
    motion: false,
    copy: false,
    allocate: false,
};

/// The per-object decision, from what the last frame saw and what this frame
/// reads. A change of the version token on an unchanged source is the evidence
/// it moves on its own; it allocates the buffer, and the frame after it has
/// motion.
///
/// - Nothing seen yet: record the tokens.
/// - No buffer, same buffer, extent and `same` token, version changed: allocate
///   and copy, no motion this frame.
/// - No buffer, anything else: record the tokens (a mesh whose triangles change
///   every frame never allocates).
/// - Buffer, the extent changed: reallocate and copy, no motion.
/// - Buffer, another source buffer or `same` token: copy, no motion.
/// - Buffer, same source, version changed: motion, then copy.
/// - Buffer, same source, version unchanged: nothing (prev equals current).
pub(super) fn advance<Same, Version>(
    last: Option<Seen<Same, Version>>,
    now: Tokens<Same, Version>,
) -> (Step, Seen<Same, Version>)
where
    Same: Copy + PartialEq,
    Version: Copy + PartialEq,
{
    let Some(last) = last else {
        return (
            IDLE,
            Seen {
                tokens: now,
                held: None,
            },
        );
    };
    let same_source = last.tokens.buffer == now.buffer
        && last.tokens.extent == now.extent
        && last.tokens.same == now.same;
    let moved = last.tokens.version != now.version;
    let (step, held) = match last.held {
        None if same_source && moved => (
            Step {
                motion: false,
                copy: true,
                allocate: true,
            },
            Some(now.extent),
        ),
        None => (IDLE, None),
        Some(held) if !same_source => (
            Step {
                motion: false,
                copy: true,
                allocate: held != now.extent,
            },
            Some(now.extent),
        ),
        Some(held) if moved => (
            Step {
                motion: true,
                copy: true,
                allocate: false,
            },
            Some(held),
        ),
        Some(held) => (IDLE, Some(held)),
    };
    (step, Seen { tokens: now, held })
}

/// One object's history across frames.
pub(super) struct History<Same, Version> {
    seen: Seen<Same, Version>,
    buffer: Option<manifold_gpu::GpuBuffer>,
}

/// An object's previous vertex positions.
pub(super) type PrevPositions = History<u64, u64>;
/// An object's previous instance records.
pub(super) type PrevInstances = History<(), ContentVersion>;

/// A draw's previous records this frame: the buffer, whether the velocity pass
/// reads it, and whether a copy refreshes it after Pass A (a history does, a
/// producer's own records do not).
pub(super) struct PrevDraw {
    buffer: manifold_gpu::GpuBuffer,
    records: u32,
    motion: bool,
    copy: bool,
}

impl PrevDraw {
    /// The buffer the draw's previous-record binding takes this frame, when
    /// the velocity pass reads it.
    pub(super) fn bound(&self) -> Option<&manifold_gpu::GpuBuffer> {
        self.motion.then_some(&self.buffer)
    }
}

/// The pack kernel, built once per device; `prewarm_pipelines` warms the same
/// pipeline.
pub(super) fn create_pack_pipeline(
    device: &manifold_gpu::GpuDevice,
) -> manifold_gpu::GpuComputePipeline {
    device.create_compute_pipeline(
        PACK_SHADER,
        "cs_main",
        "node.render_scene pack prev positions",
    )
}

/// Whether the velocity variant draws this object, so it reads its history:
/// not points, and not the single-colour transmissive pass.
fn writes_velocity(draw: &ObjectDraw<'_>, velocity_wired: bool, has_transmission: bool) -> bool {
    velocity_wired && !draw.points && !(has_transmission && draw.routes_to_transparent())
}

/// What this frame reads of a draw's mesh, when the draw writes velocity: it
/// has vertices, and its producer publishes revisions.
fn vertex_tokens(
    draw: &ObjectDraw<'_>,
    velocity_wired: bool,
    has_transmission: bool,
) -> Option<VertexTokens> {
    if !writes_velocity(draw, velocity_wired, has_transmission) || draw.vertex_count == 0 {
        return None;
    }
    let revision = draw.mesh_revision?;
    let extent = draw_extent(draw);
    (extent > 0).then(|| Tokens {
        buffer: draw.vertices.identity_key(),
        extent,
        same: revision.topology,
        version: revision.positions,
    })
}

/// What this frame reads of a draw's instance array, when the draw writes
/// velocity: a wired array with copies to draw, whose producer publishes
/// content versions. The extent is the CPU bound on the copies drawn, so a
/// producer's GPU live count never reaches past it.
fn instance_tokens(
    draw: &ObjectDraw<'_>,
    velocity_wired: bool,
    has_transmission: bool,
) -> Option<InstanceTokens> {
    if !writes_velocity(draw, velocity_wired, has_transmission) || draw.instance_count == 0 {
        return None;
    }
    Some(Tokens {
        buffer: draw.instances?.identity_key(),
        extent: draw.instance_count,
        same: (),
        version: draw.instances_content?,
    })
}

/// The producer's own previous positions for a draw that writes velocity, when
/// they cover every vertex record the draw reads.
fn provided_prev(
    draw: &ObjectDraw<'_>,
    velocity_wired: bool,
    has_transmission: bool,
) -> Option<PrevDraw> {
    let buffer = draw.provided_prev?;
    if !writes_velocity(draw, velocity_wired, has_transmission) || draw.vertex_count == 0 {
        return None;
    }
    let records = draw_extent(draw);
    (buffer.size >= u64::from(records) * RECORD_BYTES).then(|| PrevDraw {
        buffer: buffer.clone(),
        records,
        motion: true,
        copy: false,
    })
}

/// Decide one object's history for this frame: allocate or keep its buffer and
/// say what the draw does with it. An object that does not write velocity
/// (`now` is `None`) drops its history, so re-enabling velocity never reads
/// records from an older frame.
fn plan_history<Same, Version>(
    ctx: &mut EffectNodeContext<'_, '_>,
    device: &manifold_gpu::GpuDevice,
    entry: &mut Option<History<Same, Version>>,
    now: Option<Tokens<Same, Version>>,
    record_bytes: u64,
    object: u32,
    what: &str,
) -> Option<PrevDraw>
where
    Same: Copy + PartialEq,
    Version: Copy + PartialEq,
{
    let Some(now) = now else {
        *entry = None;
        return None;
    };
    let (step, mut seen) = advance(entry.as_ref().map(|e| e.seen), now);
    let mut buffer = entry.take().and_then(|e| e.buffer);
    if step.allocate {
        buffer =
            match device.try_create_buffer(u64::from(now.extent) * record_bytes, BufferUse::Gpu) {
                Ok(buffer) => Some(buffer),
                Err(error) => {
                    ctx.error(format!("object_{object}: {what} unavailable: {error}"));
                    // Forget the buffer so the next change tries again.
                    seen.held = None;
                    None
                }
            };
    }
    let prev = if step.copy {
        buffer.clone().map(|buffer| PrevDraw {
            buffer,
            records: now.extent,
            motion: step.motion,
            copy: true,
        })
    } else {
        None
    };
    *entry = Some(History { seen, buffer });
    prev
}

impl RenderScene {
    /// Decide each draw's previous-record handling for this frame, for its
    /// vertices and for its instances: allocate or keep the buffers, say
    /// whether the velocity pass reads them, and queue the copies that refresh
    /// them after Pass A.
    pub(super) fn plan_prev_positions(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        draws: &mut [ObjectDraw<'_>],
        velocity_wired: bool,
        has_transmission: bool,
    ) {
        let device = ctx.gpu_encoder().device;
        let mut vertex_copies = false;
        for draw in draws.iter_mut() {
            let slot = draw.slot as usize;
            let (Some(positions), Some(instances)) = (
                self.prev_positions.get_mut(slot),
                self.prev_instances.get_mut(slot),
            ) else {
                continue;
            };
            draw.prev_positions = match provided_prev(draw, velocity_wired, has_transmission) {
                Some(prev) => {
                    *positions = None;
                    Some(prev)
                }
                None => plan_history(
                    ctx,
                    device,
                    positions,
                    vertex_tokens(draw, velocity_wired, has_transmission),
                    RECORD_BYTES,
                    draw.slot,
                    "per-vertex motion",
                ),
            };
            if let Some(prev) = &draw.prev_positions {
                draw.uniforms.render_mode[2] = if prev.motion { 1.0 } else { 0.0 };
                vertex_copies |= prev.copy;
            }
            draw.prev_instances = plan_history(
                ctx,
                device,
                instances,
                instance_tokens(draw, velocity_wired, has_transmission),
                INSTANCE_BYTES,
                draw.slot,
                "per-instance motion",
            );
            if let Some(prev) = &draw.prev_instances {
                draw.uniforms.render_mode[3] = if prev.motion { 1.0 } else { 0.0 };
            }
        }
        if vertex_copies && self.pack_prev_positions_pipeline.is_none() {
            self.pack_prev_positions_pipeline = Some(create_pack_pipeline(device));
        }
    }

    /// Drop every object's previous vertex positions and instance records: a
    /// cut, a seek, a rebuilt scene or an empty frame leaves no earlier frame
    /// to measure motion against.
    pub(super) fn forget_prev_positions(&mut self) {
        self.prev_positions
            .iter_mut()
            .for_each(|entry| *entry = None);
        self.prev_instances
            .iter_mut()
            .for_each(|entry| *entry = None);
    }

    /// Copy this frame's vertex positions and instance records into each
    /// planned buffer. Encoded after Pass A, whose velocity pass read the
    /// previous contents.
    pub(super) fn encode_prev_position_copies(
        &self,
        ctx: &mut EffectNodeContext<'_, '_>,
        draws: &[ObjectDraw<'_>],
    ) {
        let copied = |prev: &Option<PrevDraw>| prev.as_ref().is_some_and(|prev| prev.copy);
        if !draws
            .iter()
            .any(|draw| copied(&draw.prev_positions) || copied(&draw.prev_instances))
        {
            return;
        }
        let gpu = ctx.gpu_encoder();
        for draw in draws {
            if let Some(prev) = draw.prev_positions.as_ref().filter(|prev| prev.copy) {
                let pipeline = self
                    .pack_prev_positions_pipeline
                    .as_ref()
                    .expect("created when the copies were planned");
                let args = PackArgs {
                    count: prev.records,
                    _pad: [0; 3],
                };
                gpu.native_enc.dispatch_compute(
                    pipeline,
                    &[
                        GpuBinding::Bytes {
                            binding: 0,
                            data: bytemuck::bytes_of(&args),
                        },
                        GpuBinding::Buffer {
                            binding: 1,
                            buffer: draw.vertices,
                            offset: 0,
                        },
                        GpuBinding::Buffer {
                            binding: 2,
                            buffer: &prev.buffer,
                            offset: 0,
                        },
                    ],
                    [prev.records.div_ceil(PACK_GROUP), 1, 1],
                    "node.render_scene prev positions",
                );
            }
            if let (Some(prev), Some(instances)) = (&draw.prev_instances, draw.instances) {
                gpu.native_enc.copy_buffer_to_buffer(
                    instances,
                    &prev.buffer,
                    u64::from(prev.records) * INSTANCE_BYTES,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(buffer: usize, extent: u32, topology: u64, positions: u64) -> VertexTokens {
        Tokens {
            buffer,
            extent,
            same: topology,
            version: positions,
        }
    }

    fn instances(buffer: usize, count: u32, revision: u64) -> InstanceTokens {
        Tokens {
            buffer,
            extent: count,
            same: (),
            version: ContentVersion::new(
                1,
                manifold_node_engine::exec::execution_plan::ResourceId(3),
                revision,
            ),
        }
    }

    const STEP_ALLOCATE: Step = Step {
        motion: false,
        copy: true,
        allocate: true,
    };
    const STEP_COPY: Step = Step {
        motion: false,
        copy: true,
        allocate: false,
    };
    const STEP_MOTION: Step = Step {
        motion: true,
        copy: true,
        allocate: false,
    };

    #[test]
    fn a_mesh_seen_for_the_first_time_only_records_its_tokens() {
        let now = tokens(0xA0, 3000, 7, 40);
        let (step, seen) = advance(None, now);
        assert_eq!(step, IDLE);
        assert_eq!(
            seen,
            Seen {
                tokens: now,
                held: None
            }
        );
    }

    #[test]
    fn a_mesh_that_never_changes_never_allocates() {
        let now = tokens(0xA0, 3000, 7, 40);
        let (_, mut seen) = advance(None, now);
        for _ in 0..4 {
            let (step, next) = advance(Some(seen), now);
            assert_eq!(step, IDLE);
            assert_eq!(next.held, None);
            seen = next;
        }
    }

    #[test]
    fn the_first_position_change_allocates_and_the_next_one_has_motion() {
        let (_, seen) = advance(None, tokens(0xA0, 3000, 7, 40));
        let (step, seen) = advance(Some(seen), tokens(0xA0, 3000, 7, 41));
        assert_eq!(step, STEP_ALLOCATE);
        assert_eq!(seen.held, Some(3000));
        let (step, seen) = advance(Some(seen), tokens(0xA0, 3000, 7, 42));
        assert_eq!(step, STEP_MOTION);
        assert_eq!(seen.held, Some(3000));
        // Positions that did not change draw with prev == current and copy nothing.
        let (step, seen) = advance(Some(seen), tokens(0xA0, 3000, 7, 42));
        assert_eq!(step, IDLE);
        assert_eq!(seen.held, Some(3000));
        // The buffer survives the pause: the next change has motion again.
        let (step, _) = advance(Some(seen), tokens(0xA0, 3000, 7, 45));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn a_mesh_whose_triangles_change_every_frame_never_allocates() {
        let (_, mut seen) = advance(None, tokens(0xA0, 3000, 1, 1));
        for frame in 2..8 {
            let (step, next) = advance(Some(seen), tokens(0xA0, 3000, frame, frame));
            assert_eq!(step, IDLE, "frame {frame}");
            assert_eq!(next.held, None);
            seen = next;
        }
    }

    #[test]
    fn a_topology_change_copies_without_motion_and_keeps_the_buffer() {
        let held = Seen {
            tokens: tokens(0xA0, 3000, 7, 40),
            held: Some(3000),
        };
        let (step, seen) = advance(Some(held), tokens(0xA0, 3000, 8, 41));
        assert_eq!(step, STEP_COPY);
        assert_eq!(seen.held, Some(3000));
        // The frame after sees stable triangles again.
        let (step, _) = advance(Some(seen), tokens(0xA0, 3000, 8, 42));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn another_vertex_buffer_of_the_same_size_copies_without_motion() {
        let held = Seen {
            tokens: tokens(0xA0, 3000, 7, 40),
            held: Some(3000),
        };
        let (step, seen) = advance(Some(held), tokens(0xB0, 3000, 7, 41));
        assert_eq!(step, STEP_COPY);
        assert_eq!(seen.tokens.buffer, 0xB0);
    }

    #[test]
    fn a_resized_mesh_reallocates_for_its_new_extent() {
        let held = Seen {
            tokens: tokens(0xA0, 3000, 7, 40),
            held: Some(3000),
        };
        let (step, seen) = advance(Some(held), tokens(0xB0, 4500, 7, 41));
        assert_eq!(step, STEP_ALLOCATE);
        assert_eq!(seen.held, Some(4500));
        let (step, _) = advance(Some(seen), tokens(0xB0, 4500, 7, 42));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn a_changed_mesh_without_a_buffer_only_records_the_new_tokens() {
        let (_, seen) = advance(None, tokens(0xA0, 3000, 7, 40));
        let now = tokens(0xB0, 999, 8, 41);
        let (step, seen) = advance(Some(seen), now);
        assert_eq!(step, IDLE);
        assert_eq!(seen.tokens, now);
        assert_eq!(seen.held, None);
    }

    #[test]
    fn an_indexed_draw_reads_every_record_and_a_plain_draw_its_bound() {
        // 12 indices over a 7-record buffer: the index count is not the extent.
        assert_eq!(read_extent(true, 12, 7), 7);
        assert_eq!(read_extent(true, 0, 7), 7);
        // A plain draw reads its whole-triangle bound, never past the buffer.
        assert_eq!(read_extent(false, 6, 7), 6);
        assert_eq!(read_extent(false, 9, 7), 7);
    }

    #[test]
    fn copies_that_never_change_never_allocate() {
        let now = instances(0xC0, 20_000, 5);
        let (step, mut seen) = advance(None, now);
        assert_eq!(step, IDLE);
        for _ in 0..4 {
            let (step, next) = advance(Some(seen), now);
            assert_eq!(step, IDLE);
            assert_eq!(next.held, None);
            seen = next;
        }
    }

    #[test]
    fn the_first_content_change_of_copies_allocates_and_the_next_one_has_motion() {
        let (_, seen) = advance(None, instances(0xC0, 20_000, 5));
        let (step, seen) = advance(Some(seen), instances(0xC0, 20_000, 6));
        assert_eq!(step, STEP_ALLOCATE);
        assert_eq!(seen.held, Some(20_000));
        let (step, seen) = advance(Some(seen), instances(0xC0, 20_000, 7));
        assert_eq!(step, STEP_MOTION);
        // Copies that did not change draw with prev == current and copy nothing.
        let (step, seen) = advance(Some(seen), instances(0xC0, 20_000, 7));
        assert_eq!(step, IDLE);
        assert_eq!(seen.held, Some(20_000));
        // The buffer survives the pause: the next change has motion again.
        let (step, _) = advance(Some(seen), instances(0xC0, 20_000, 11));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn copies_in_another_buffer_copy_without_motion_and_keep_the_buffer() {
        let held = Seen {
            tokens: instances(0xC0, 20_000, 5),
            held: Some(20_000),
        };
        let (step, seen) = advance(Some(held), instances(0xD0, 20_000, 6));
        assert_eq!(step, STEP_COPY);
        assert_eq!(seen.held, Some(20_000));
        assert_eq!(seen.tokens.buffer, 0xD0);
        let (step, _) = advance(Some(seen), instances(0xD0, 20_000, 7));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn a_new_copy_count_reallocates_for_it_without_motion() {
        let held = Seen {
            tokens: instances(0xC0, 20_000, 5),
            held: Some(20_000),
        };
        let (step, seen) = advance(Some(held), instances(0xC0, 30_000, 6));
        assert_eq!(step, STEP_ALLOCATE);
        assert_eq!(seen.held, Some(30_000));
        let (step, seen) = advance(Some(seen), instances(0xC0, 12_000, 7));
        assert_eq!(step, STEP_ALLOCATE);
        assert_eq!(seen.held, Some(12_000));
        let (step, _) = advance(Some(seen), instances(0xC0, 12_000, 8));
        assert_eq!(step, STEP_MOTION);
    }

    #[test]
    fn changed_copies_of_another_source_without_a_buffer_only_record_their_tokens() {
        let (_, seen) = advance(None, instances(0xC0, 20_000, 5));
        let now = instances(0xD0, 64, 6);
        let (step, seen) = advance(Some(seen), now);
        assert_eq!(step, IDLE);
        assert_eq!(seen.tokens, now);
        assert_eq!(seen.held, None);
    }
}
