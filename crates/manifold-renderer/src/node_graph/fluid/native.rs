use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::Seconds;
use manifold_fluids::{Bounds, FluidWorld, FrameStats, SurfaceVertex, WhitewaterParticle};
use manifold_physics::FieldInput;

use crate::generators::mesh_common::MeshVertex;

use super::impulses::ImpulseSum;
use super::roles;
use super::{FluidRuntime, Reply, Request, cancelled_reply};
use crate::node_graph::fluid_cache::{CacheMode, CacheReader, CacheWriter};

struct PreparedTick<'request> {
    next_obstacle: super::Transform,
    field: super::ContinuousField<'request>,
    impulse: ImpulseSum<'request>,
}

impl PreparedTick<'_> {
    fn fields(&self) -> [FieldInput<'_>; 2] {
        [
            FieldInput {
                field: &self.field,
                acceleration: 1.0,
                delta_velocity: 0.0,
            },
            FieldInput {
                field: &self.impulse,
                acceleration: 0.0,
                delta_velocity: 1.0,
            },
        ]
    }
}

/// Native FLIP and cache ownership stays together on the worker thread.
#[derive(Default)]
pub(super) struct NativeSimulation {
    world: Option<(u64, FluidWorld)>,
    native_roles: roles::NativeRoles,
    surface: Vec<SurfaceVertex>,
    whitewater: Vec<WhitewaterParticle>,
    writer: Option<CacheWriter>,
    playback: Option<CacheReader>,
    cache_epoch: Option<u64>,
}

impl NativeSimulation {
    fn prepare_world(
        &mut self,
        request: &Request,
        domain: super::FluidDomainLayout,
    ) -> Result<(), String> {
        if self
            .world
            .as_ref()
            .is_some_and(|(epoch, _)| *epoch == request.epoch)
        {
            return Ok(());
        }

        // Dropping/rebuilding an old world can take time too; keep it off the
        // content thread along with all native work.
        self.world = None;
        let mut new =
            FluidWorld::new(domain.config(request.settings)).map_err(|e| e.to_string())?;
        new.set_liquid_options(request.settings.liquid)
            .map_err(|e| e.to_string())?;
        new.set_time_step_options(request.settings.time_steps)
            .map_err(|e| e.to_string())?;
        new.set_surface_options(request.settings.surface)
            .map_err(|e| e.to_string())?;
        new.set_whitewater_options(request.settings.whitewater)
            .map_err(|e| e.to_string())?;
        new.set_boundary_collisions(request.settings.boundary_collisions)
            .map_err(|e| e.to_string())?;
        if request.settings.fill_height > 0.0 {
            new.add_fluid_box(
                Bounds {
                    min: [0.0; 3],
                    max: [domain.size[0], request.settings.fill_height, domain.size[2]],
                },
                [0.0; 3],
            )
            .map_err(|e| e.to_string())?;
        }
        if let Some(volume) = request.settings.initial_volume {
            new.add_fluid_box(domain.bounds(volume), [0.0; 3])
                .map_err(|e| e.to_string())?;
        }
        self.native_roles = roles::NativeRoles::prepare(&mut new, &request.role_setup, domain)?;
        self.world = Some((request.epoch, new));
        Ok(())
    }

    fn prepare_tick<'request>(
        native_roles: &roles::NativeRoles,
        native: &mut FluidWorld,
        request: &'request Request,
        domain: super::FluidDomainLayout,
        tick: u64,
    ) -> Result<PreparedTick<'request>, String> {
        let step = FluidRuntime::step_at(&request.history, tick);
        native
            .set_gravity([0.0, step.current.gravity, 0.0])
            .map_err(|e| e.to_string())?;
        native
            .set_emitter(
                domain.bounds(step.current.emitter),
                [0.0, -step.current.inflow_speed, 0.0],
                step.current.emission,
            )
            .map_err(|e| e.to_string())?;
        if step.current.obstacle_enabled {
            native
                .set_obstacle(
                    domain.bounds(step.previous.obstacle),
                    domain.bounds(step.current.obstacle),
                    domain.bounds(step.next.obstacle),
                )
                .map_err(|e| e.to_string())?;
        } else {
            native.clear_obstacle().map_err(|e| e.to_string())?;
        }
        native_roles.apply(
            native,
            &request.role_setup,
            &request.history,
            &request.role_history,
            tick,
            domain,
        )?;
        let field = FluidRuntime::field_at(&request.history, tick, domain);
        let begin = request
            .impulses
            .partition_point(|event| event.applied.tick < tick);
        let end = request
            .impulses
            .partition_point(|event| event.applied.tick <= tick);
        let impulse = ImpulseSum {
            events: &request.impulses[begin..end],
            origin: domain.min,
        };
        Ok(PreparedTick {
            next_obstacle: step.next.obstacle,
            field,
            impulse,
        })
    }

    fn capture_output(
        &mut self,
        request: &mut Request,
        domain: super::FluidDomainLayout,
        tick: u64,
        pose: super::Transform,
        stats: FrameStats,
        last: bool,
    ) -> Result<(), String> {
        if request.cache_mode != CacheMode::Record && !last {
            return Ok(());
        }
        let native = &mut self.world.as_mut().expect("world initialized").1;
        native
            .surface(&mut self.surface)
            .map_err(|e| e.to_string())?;
        if self.surface.len() > request.settings.max_vertices {
            return Err(format!(
                "Water surface needs {} vertices; capacity is {}. Lower resolution/detail or increase mesh capacity and reset.",
                self.surface.len(),
                request.settings.max_vertices
            ));
        }
        request
            .recycle
            .extend(self.surface.iter().map(|v| MeshVertex {
                position: domain.to_scene(v.position),
                _pad0: 0.0,
                normal: v.normal,
                _pad1: 0.0,
                uv: [
                    v.position[0] / domain.size[0],
                    v.position[2] / domain.size[2],
                ],
                _pad2: [0.0; 2],
                tangent: [0.0; 4],
                color: [1.0; 4],
            }));
        if request.settings.whitewater.enabled {
            native
                .whitewater(&mut self.whitewater)
                .map_err(|e| e.to_string())?;
            if self.whitewater.len() > request.settings.whitewater.max_particles as usize {
                return Err("Water whitewater snapshot exceeds its configured capacity".into());
            }
            request.recycle_whitewater.fill(&self.whitewater, domain);
        }
        if request.cache_mode == CacheMode::Record {
            self.writer
                .as_ref()
                .expect("record writer initialized")
                .append(
                    tick + 1,
                    &request.recycle,
                    &request.recycle_whitewater,
                    pose,
                    stats,
                )?;
        }
        if !last {
            request.recycle.clear();
            request.recycle_whitewater.clear();
        }
        Ok(())
    }

    pub(super) fn process(&mut self, mut request: Request, cancel_epoch: &AtomicU64) -> Reply {
        if cancel_epoch.load(Ordering::Acquire) != request.epoch {
            return cancelled_reply(request);
        }
        let mut stats = FrameStats::default();
        let mut pose = request.initial.obstacle;
        let mut setup_error = None;
        let mut completed_count = 0usize;
        let mut started_tick = request.start_tick;
        if self.cache_epoch != Some(request.epoch) {
            self.writer = None;
            self.world = None;
            self.playback = None;
            self.cache_epoch = Some(request.epoch);
            if setup_error.is_none() {
                match request.cache_mode {
                    CacheMode::Live => {}
                    CacheMode::Record => {
                        match CacheWriter::create(request.cache_path.clone(), request.settings) {
                            Ok(new_writer) => self.writer = Some(new_writer),
                            Err(error) => setup_error = Some(error),
                        }
                    }
                    CacheMode::Playback => {
                        match CacheReader::open(request.cache_path.clone(), request.settings) {
                            Ok(reader) => self.playback = Some(reader),
                            Err(error) => setup_error = Some(error),
                        }
                    }
                }
            }
        }
        if cancel_epoch.load(Ordering::Acquire) != request.epoch {
            return cancelled_reply(request);
        }
        let result = (|| -> Result<(), String> {
            if let Some(error) = setup_error {
                return Err(error);
            }
            if request.cache_mode == CacheMode::Playback {
                request.recycle.clear();
                request.recycle_whitewater.clear();
                if request.count > 0 {
                    let cache = self.playback.as_ref().expect("playback initialized");
                    (pose, stats) = cache.read_into(
                        request.start_tick + request.count as u64,
                        &mut request.recycle,
                        &mut request.recycle_whitewater,
                    )?;
                    completed_count = request.count;
                    stats.simulation_ms = 0.0;
                    stats.meshing_ms = 0.0;
                }
                return Ok(());
            }
            let domain = request.settings.domain_layout()?;
            self.prepare_world(&request, domain)?;
            request.recycle.clear();
            request
                .recycle_whitewater
                .prepare(if request.settings.whitewater.enabled {
                    request.settings.whitewater.max_particles as usize
                } else {
                    0
                });
            for index in 0..request.count {
                if cancel_epoch.load(Ordering::Acquire) != request.epoch {
                    break;
                }
                let tick = request.start_tick + index as u64;
                started_tick = tick + 1;
                let (tick_stats, next_obstacle) = {
                    let native = &mut self.world.as_mut().expect("world initialized").1;
                    let prepared =
                        Self::prepare_tick(&self.native_roles, native, &request, domain, tick)?;
                    let tick_stats =
                        if prepared.field.is_empty() && prepared.impulse.events.is_empty() {
                            native.step(Seconds(super::TICK))
                        } else {
                            native.step_with_fields(Seconds(super::TICK), &prepared.fields())
                        }
                        .map_err(|e| e.to_string())?;
                    (tick_stats, prepared.next_obstacle)
                };
                stats = tick_stats;
                completed_count += 1;
                pose = next_obstacle;
                let last = index + 1 == request.count;
                self.capture_output(&mut request, domain, tick, pose, stats, last)?;
            }
            Ok(())
        })();
        Reply {
            epoch: request.epoch,
            tick: request.start_tick + completed_count as u64,
            started_tick,
            impulses: request.impulses,
            history: request.history,
            role_history: request.role_history,
            vertices: request.recycle,
            whitewater: request.recycle_whitewater,
            obstacle: pose,
            stats,
            error: result.err(),
        }
    }
}
