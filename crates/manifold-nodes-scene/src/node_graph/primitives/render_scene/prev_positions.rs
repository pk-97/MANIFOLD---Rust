//! Per-vertex motion for deforming meshes.
//!
//! The velocity pass projects each vertex through the previous frame's object,
//! instance and camera transforms, so a vertex that moves on its own has no
//! motion of its own. An object whose mesh deforms with the same triangles (its
//! producer advances the positions token and holds the topology token) keeps
//! last frame's vertex positions in a buffer: the velocity pass reads them in
//! place of the current positions, and a compute pass after Pass A refreshes
//! them. A mesh that never changes never allocates one, and none of this runs
//! for a draw that does not write velocity.

use super::*;

/// One previous-position record: `vec4<f32>` (xyz, w = 1).
pub(super) const RECORD_BYTES: u64 = 16;
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

/// What identifies the vertices a draw reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Tokens {
    /// The vertex buffer, and how many of its records the draw reads.
    buffer: usize,
    extent: u32,
    topology: u64,
    positions: u64,
}

/// What the last frame that drew this object left behind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Seen {
    tokens: Tokens,
    /// Records the previous-position buffer holds; `None` until the mesh
    /// first deforms.
    held: Option<u32>,
}

/// What this frame does for one draw.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Step {
    /// The velocity pass reads last frame's positions from the buffer.
    motion: bool,
    /// A compute pass after Pass A copies this frame's positions into it.
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
/// reads. A change of the positions token on an unchanged mesh is the evidence
/// it deforms; it allocates the buffer, and the frame after it has motion.
///
/// - Nothing seen yet: record the tokens.
/// - No buffer, same vertex buffer and triangles, positions changed: allocate
///   and copy, no motion this frame.
/// - No buffer, anything else: record the tokens (a mesh whose triangles change
///   every frame never allocates).
/// - Buffer, the vertex buffer's extent changed: reallocate and copy, no motion.
/// - Buffer, another vertex buffer or other triangles: copy, no motion.
/// - Buffer, same mesh, positions changed: motion, then copy.
/// - Buffer, same mesh, positions unchanged: nothing (prev equals current).
pub(super) fn advance(last: Option<Seen>, now: Tokens) -> (Step, Seen) {
    let Some(last) = last else {
        return (
            IDLE,
            Seen {
                tokens: now,
                held: None,
            },
        );
    };
    let same_mesh = last.tokens.buffer == now.buffer
        && last.tokens.extent == now.extent
        && last.tokens.topology == now.topology;
    let moved = last.tokens.positions != now.positions;
    let (step, held) = match last.held {
        None if same_mesh && moved => (
            Step {
                motion: false,
                copy: true,
                allocate: true,
            },
            Some(now.extent),
        ),
        None => (IDLE, None),
        Some(held) if !same_mesh => (
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
pub(super) struct PrevPositions {
    seen: Seen,
    buffer: Option<manifold_gpu::GpuBuffer>,
}

/// A draw's previous positions this frame: the buffer the copy refreshes after
/// Pass A, and whether the velocity pass reads it.
pub(super) struct PrevDraw {
    buffer: manifold_gpu::GpuBuffer,
    records: u32,
    motion: bool,
}

impl PrevDraw {
    /// The buffer `@binding(56)` takes this frame, when the velocity pass reads it.
    pub(super) fn bound(&self) -> Option<&manifold_gpu::GpuBuffer> {
        self.motion.then_some(&self.buffer)
    }
}

/// The pack kernel, built once per device; `prewarm_pipelines` warms the same
/// pipeline.
pub(super) fn create_pack_pipeline(device: &manifold_gpu::GpuDevice) -> manifold_gpu::GpuComputePipeline {
    device.create_compute_pipeline(PACK_SHADER, "cs_main", "node.render_scene pack prev positions")
}

/// What this frame reads of a draw's mesh, when the draw writes velocity:
/// the velocity variant draws it (not points, and not the single-colour
/// transmissive pass), it has vertices, and its producer publishes revisions.
fn tokens(draw: &ObjectDraw<'_>, velocity_wired: bool, has_transmission: bool) -> Option<Tokens> {
    let writes_velocity =
        velocity_wired && !draw.points && !(has_transmission && draw.routes_to_transparent());
    if !writes_velocity || draw.vertex_count == 0 {
        return None;
    }
    let revision = draw.mesh_revision?;
    let records = (draw.vertices.size / std::mem::size_of::<MeshVertex>() as u64)
        .min(u64::from(u32::MAX)) as u32;
    let extent = read_extent(draw.indices.is_some(), draw.vertex_count, records);
    (extent > 0).then(|| Tokens {
        buffer: draw.vertices.identity_key(),
        extent,
        topology: revision.topology,
        positions: revision.positions,
    })
}

impl RenderScene {
    /// Decide each draw's previous-position handling for this frame: allocate
    /// or keep its buffer, say whether the velocity pass reads it, and queue the
    /// copy that refreshes it after Pass A. A draw that does not write velocity
    /// drops its history, so re-enabling velocity never reads positions from an
    /// older frame.
    pub(super) fn plan_prev_positions(
        &mut self,
        ctx: &mut EffectNodeContext<'_, '_>,
        draws: &mut [ObjectDraw<'_>],
        velocity_wired: bool,
        has_transmission: bool,
    ) {
        let device = ctx.gpu_encoder().device;
        let mut copies = false;
        for draw in draws.iter_mut() {
            let Some(entry) = self.prev_positions.get_mut(draw.slot as usize) else {
                continue;
            };
            let Some(now) = tokens(draw, velocity_wired, has_transmission) else {
                *entry = None;
                continue;
            };
            let (step, mut seen) = advance(entry.as_ref().map(|e| e.seen), now);
            let mut buffer = entry.take().and_then(|e| e.buffer);
            if step.allocate {
                buffer = match device.try_create_buffer(u64::from(now.extent) * RECORD_BYTES) {
                    Ok(buffer) => Some(buffer),
                    Err(error) => {
                        ctx.error(format!(
                            "object_{}: per-vertex motion unavailable: {error}",
                            draw.slot
                        ));
                        // Forget the buffer so the next change tries again.
                        seen.held = None;
                        None
                    }
                };
            }
            if step.copy
                && let Some(buffer) = buffer.clone()
            {
                draw.uniforms.render_mode[2] = if step.motion { 1.0 } else { 0.0 };
                draw.prev_positions = Some(PrevDraw {
                    buffer,
                    records: now.extent,
                    motion: step.motion,
                });
                copies = true;
            }
            *entry = Some(PrevPositions { seen, buffer });
        }
        if copies && self.pack_prev_positions_pipeline.is_none() {
            self.pack_prev_positions_pipeline = Some(create_pack_pipeline(device));
        }
    }

    /// Drop every object's previous positions: a cut, a seek, a rebuilt scene
    /// or an empty frame leaves no earlier frame to measure motion against.
    pub(super) fn forget_prev_positions(&mut self) {
        self.prev_positions.iter_mut().for_each(|entry| *entry = None);
    }

    /// Copy this frame's vertex positions into each planned buffer. Encoded
    /// after Pass A, whose velocity pass read the previous contents.
    pub(super) fn encode_prev_position_copies(
        &self,
        ctx: &mut EffectNodeContext<'_, '_>,
        draws: &[ObjectDraw<'_>],
    ) {
        let mut copies = draws
            .iter()
            .filter_map(|draw| draw.prev_positions.as_ref().map(|prev| (draw, prev)))
            .peekable();
        if copies.peek().is_none() {
            return;
        }
        let pipeline = self
            .pack_prev_positions_pipeline
            .as_ref()
            .expect("created when the copies were planned");
        let gpu = ctx.gpu_encoder();
        for (draw, prev) in copies {
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(buffer: usize, extent: u32, topology: u64, positions: u64) -> Tokens {
        Tokens {
            buffer,
            extent,
            topology,
            positions,
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
}
