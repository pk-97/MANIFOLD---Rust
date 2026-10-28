//! Automatic hazard tracking inside one command buffer (design section 5).
//! Every command names what it reads and writes; before it records, the
//! tracker turns read-after-write and write-after-write into memory
//! dependencies and write-after-read into an execution dependency, merged
//! into one global `vkCmdPipelineBarrier2`. Images stay in `GENERAL`, so no
//! barrier carries a layout. Each command buffer starts with a global
//! barrier, so the tracker starts empty with it.

use std::collections::HashMap;

use ash::vk;
use ash::vk::{AccessFlags2 as A, PipelineStageFlags2 as S};

#[derive(Clone, Copy, Default)]
struct State {
    /// The last write: its stage and access.
    write: Option<(S, A)>,
    /// Stages that read since that write.
    read_stages: S,
    /// Stages the write has been made visible to.
    visible: S,
}

/// One root resource's accesses since the command buffer began, keyed by its
/// identity (a buffer, or an image whatever view touched it).
#[derive(Default)]
pub(crate) struct Tracker {
    states: HashMap<usize, State>,
    src_stage: S,
    src_access: A,
    dst_stage: S,
    dst_access: A,
}

impl Tracker {
    /// The next command reads `key` at `stage`.
    pub(crate) fn read(&mut self, key: usize, stage: S) {
        let state = self.states.entry(key).or_default();
        if let Some((write_stage, write_access)) = state.write
            && !state.visible.contains(stage)
        {
            self.src_stage |= write_stage;
            self.src_access |= write_access;
            self.dst_stage |= stage;
            self.dst_access |= A::MEMORY_READ;
            state.visible |= stage;
        }
        state.read_stages |= stage;
    }

    /// The next command writes `key` at `stage` with `access` (and may read
    /// it too: read-write storage counts as a write).
    pub(crate) fn write(&mut self, key: usize, stage: S, access: A) {
        let state = self.states.entry(key).or_default();
        if let Some((write_stage, write_access)) = state.write {
            self.src_stage |= write_stage;
            self.src_access |= write_access;
            self.dst_stage |= stage;
            self.dst_access |= A::MEMORY_READ | A::MEMORY_WRITE;
        }
        if !state.read_stages.is_empty() {
            self.src_stage |= state.read_stages;
            self.dst_stage |= stage;
        }
        *state = State {
            write: Some((stage, access)),
            read_stages: S::empty(),
            visible: S::empty(),
        };
    }

    /// Record the barrier the pending accesses need, if any.
    pub(crate) fn flush(&mut self, device: &ash::Device, cmd: vk::CommandBuffer) {
        if self.src_stage.is_empty() {
            return;
        }
        let barrier = [vk::MemoryBarrier2::default()
            .src_stage_mask(self.src_stage)
            .src_access_mask(self.src_access)
            .dst_stage_mask(self.dst_stage)
            .dst_access_mask(self.dst_access)];
        unsafe {
            device.cmd_pipeline_barrier2(
                cmd,
                &vk::DependencyInfo::default().memory_barriers(&barrier),
            )
        };
        self.src_stage = S::empty();
        self.src_access = A::empty();
        self.dst_stage = S::empty();
        self.dst_access = A::empty();
    }

    /// A new command buffer: its opening global barrier covers everything.
    pub(crate) fn reset(&mut self) {
        self.states.clear();
        self.src_stage = S::empty();
        self.src_access = A::empty();
        self.dst_stage = S::empty();
        self.dst_access = A::empty();
    }

    /// An explicit global barrier between compute dispatches covers every
    /// compute write so far.
    pub(crate) fn compute_barrier_recorded(&mut self) {
        for state in self.states.values_mut() {
            if state
                .write
                .is_some_and(|(stage, _)| stage == S::COMPUTE_SHADER)
            {
                state.visible |= S::COMPUTE_SHADER;
            }
            state.read_stages &= !S::COMPUTE_SHADER;
        }
    }
}
