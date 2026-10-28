//! Gated templates (design section 7). P1 keeps the round a template's body
//! walks and encodes it `copies` times as plain indirect dispatches, each
//! reading its own gate, so the CPU encodes every round; the kernels guard
//! rounds after the GPU decided to stop. P6 records the round once.

use std::sync::atomic::{AtomicU64, Ordering};

use super::*;
use crate::*;

/// A binding a recorded round owns until it executes.
enum OwnedBinding {
    Buffer {
        binding: u32,
        buffer: GpuBuffer,
        offset: u64,
    },
    Texture {
        binding: u32,
        texture: GpuTexture,
    },
    Sampler {
        binding: u32,
        sampler: GpuSampler,
    },
    Bytes {
        binding: u32,
        data: Vec<u8>,
    },
}

impl OwnedBinding {
    fn new(binding: &GpuBinding) -> Self {
        match *binding {
            GpuBinding::Buffer {
                binding,
                buffer,
                offset,
            } => Self::Buffer {
                binding,
                buffer: buffer.clone(),
                offset,
            },
            GpuBinding::Texture { binding, texture } => Self::Texture {
                binding,
                texture: texture.clone(),
            },
            GpuBinding::Sampler { binding, sampler } => Self::Sampler {
                binding,
                sampler: sampler.clone(),
            },
            GpuBinding::Bytes { binding, data } => Self::Bytes {
                binding,
                data: data.to_vec(),
            },
        }
    }
}

/// One gated dispatch of a recorded round.
struct Command {
    pipeline: GpuComputePipeline,
    bindings: Vec<OwnedBinding>,
    gate: GpuBuffer,
    gate_offset: u64,
}

pub struct GatedRecorder<'s> {
    round: &'s mut Vec<Command>,
}

pub struct TemplateTicket {
    store: u64,
    round: Vec<Command>,
    copies: u32,
    /// Chunks of rounds, as `template_chunks` cuts them on every backend.
    executes: u32,
}

pub struct GpuTemplateStore {
    id: u64,
    stats: GpuTemplateStats,
}

impl GatedRecorder<'_> {
    /// One gated dispatch of the round: `groups` are its threadgroup counts
    /// when its round runs; on Vulkan the gate's own counts decide.
    pub fn dispatch_gated(
        &mut self,
        pipeline: &GpuComputePipeline,
        bindings: &[GpuBinding],
        _groups: [u32; 3],
        gate: &GpuBuffer,
        gate_offset: u64,
        _label: &str,
    ) {
        self.round.push(Command {
            pipeline: pipeline.clone(),
            bindings: bindings.iter().map(OwnedBinding::new).collect(),
            gate: gate.clone(),
            gate_offset,
        });
    }
}

impl GpuTemplateStore {
    pub fn new(_device: &GpuDevice) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            stats: GpuTemplateStats::default(),
        }
    }

    pub fn stats(&self) -> GpuTemplateStats {
        self.stats
    }

    /// Slots held, idle or in flight: none, P1 keeps no recordings.
    pub fn slots(&self) -> usize {
        0
    }
}

impl GpuEncoder {
    /// Walk `body` once as one round of exactly `commands` gated
    /// dispatches the GPU runs `copies` times. `Err`, keeping nothing, when
    /// the shape does not fit `at`'s range buffer or the body issues a
    /// different number of dispatches, as on Metal.
    pub fn prepare_template(
        &mut self,
        store: &mut GpuTemplateStore,
        at: TemplateRanges<'_>,
        commands: u32,
        copies: u32,
        body: impl FnOnce(&mut GatedRecorder<'_>) -> Result<(), String>,
    ) -> Result<TemplateTicket, String> {
        let result = prepare(store, at, commands, copies, body);
        if result.is_err() {
            store.stats.refused += 1;
        }
        result
    }

    /// Encode a prepared template's `copies` rounds here, each dispatch an
    /// indirect dispatch from its gate. Bytes upload once per execute, so
    /// every round binds the same descriptor set. `Err`, encoding nothing,
    /// for a ticket from another store.
    pub fn execute_template(
        &mut self,
        store: &mut GpuTemplateStore,
        ticket: TemplateTicket,
    ) -> Result<(), String> {
        if ticket.store != store.id {
            return Err("gated template: the ticket belongs to another store".into());
        }
        store.stats.executes += u64::from(ticket.executes);
        store.stats.command_writes += ticket.round.len() as u64 * u64::from(ticket.copies);
        let uploads: Vec<Vec<Option<(GpuBuffer, u64)>>> = ticket
            .round
            .iter()
            .map(|command| {
                command
                    .bindings
                    .iter()
                    .map(|binding| match binding {
                        OwnedBinding::Bytes { data, .. } => Some(self.ring_upload(data)),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        let bindings: Vec<Vec<GpuBinding>> = ticket
            .round
            .iter()
            .zip(&uploads)
            .map(|(command, uploads)| {
                command
                    .bindings
                    .iter()
                    .zip(uploads)
                    .map(|(binding, upload)| match (binding, upload) {
                        (
                            OwnedBinding::Buffer {
                                binding,
                                buffer,
                                offset,
                            },
                            _,
                        ) => GpuBinding::Buffer {
                            binding: *binding,
                            buffer,
                            offset: *offset,
                        },
                        (OwnedBinding::Texture { binding, texture }, _) => GpuBinding::Texture {
                            binding: *binding,
                            texture,
                        },
                        (OwnedBinding::Sampler { binding, sampler }, _) => GpuBinding::Sampler {
                            binding: *binding,
                            sampler,
                        },
                        (OwnedBinding::Bytes { binding, .. }, Some((buffer, offset))) => {
                            GpuBinding::Buffer {
                                binding: *binding,
                                buffer,
                                offset: *offset,
                            }
                        }
                        (OwnedBinding::Bytes { .. }, None) => unreachable!("bytes uploaded above"),
                    })
                    .collect()
            })
            .collect();
        for _ in 0..ticket.copies {
            for (command, bindings) in ticket.round.iter().zip(&bindings) {
                self.dispatch_compute_indirect(
                    &command.pipeline,
                    bindings,
                    &command.gate,
                    command.gate_offset,
                    "template round",
                );
            }
        }
        Ok(())
    }
}

fn prepare(
    store: &mut GpuTemplateStore,
    at: TemplateRanges<'_>,
    commands: u32,
    copies: u32,
    body: impl FnOnce(&mut GatedRecorder<'_>) -> Result<(), String>,
) -> Result<TemplateTicket, String> {
    // Vulkan encodes rounds as dispatches, so no command buffer caps an execute.
    let executes = crate::replay::template_executes(&at, commands, copies, u32::MAX)?;
    store.stats.walks += 1;
    let mut round = Vec::with_capacity(commands as usize);
    body(&mut GatedRecorder { round: &mut round })?;
    if round.len() != commands as usize {
        return Err(format!(
            "gated template: the body issued {} dispatches, not the {commands} declared",
            round.len()
        ));
    }
    Ok(TemplateTicket {
        store: store.id,
        round,
        copies,
        executes,
    })
}
