use std::sync::atomic::{AtomicU64, Ordering};

use manifold_core::Seconds;
use manifold_fluids::{
    Bounds, CaptureError, FluidWorld, FrameStats, SurfaceVertex, WhitewaterParticle,
};
use manifold_physics::FieldInput;
use manifold_physics::stepping::StepInterval;

use crate::mesh::MeshVertex;

use super::impulses::ImpulseSum;
use super::{FluidRuntime, Reply, Request, cancelled_reply};
use super::{coupled, roles};
use crate::water::fluid_cache::{CacheMode, CacheReader, CacheWriter};

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

/// A native world with a scene's settings and its initial liquid: the fill
/// and the initial volume, before roles or coupling.
pub(super) fn seeded_world(
    settings: super::FluidSettings,
    domain: super::FluidDomainLayout,
    surface_meshing: bool,
) -> Result<FluidWorld, String> {
    let mut new = FluidWorld::new_seeded(domain.config(settings), settings.seed)
        .map_err(|e| e.to_string())?;
    new.set_liquid_options(settings.liquid)
        .map_err(|e| e.to_string())?;
    new.set_time_step_options(settings.time_steps)
        .map_err(|e| e.to_string())?;
    new.set_surface_options(settings.surface)
        .map_err(|e| e.to_string())?;
    new.set_surface_reconstruction_enabled(surface_meshing)
        .map_err(|e| e.to_string())?;
    new.set_whitewater_options(settings.whitewater)
        .map_err(|e| e.to_string())?;
    new.set_boundary_collisions(settings.boundary_collisions)
        .map_err(|e| e.to_string())?;
    if settings.fill_height > 0.0 {
        let min = domain.to_native(domain.min);
        new.add_fluid_box(
            Bounds {
                min,
                max: [
                    min[0] + domain.size[0],
                    min[1] + settings.fill_height,
                    min[2] + domain.size[2],
                ],
            },
            [0.0; 3],
        )
        .map_err(|e| e.to_string())?;
    }
    if let Some(volume) = settings.initial_volume {
        new.add_fluid_box(domain.bounds(volume), [0.0; 3])
            .map_err(|e| e.to_string())?;
    }
    Ok(new)
}

/// Native FLIP and cache ownership stays together on the worker thread.
#[derive(Default)]
pub(super) struct NativeSimulation {
    world: Option<(u64, FluidWorld)>,
    native_roles: roles::NativeRoles,
    surface: Vec<SurfaceVertex>,
    whitewater: Vec<WhitewaterParticle>,
    writer: Option<CacheWriter>,
    take_writer: Option<super::take::Writer>,
    playback: Option<CacheReader>,
    cache_epoch: Option<u64>,
    coupled: Option<coupled::Native>,
}

impl NativeSimulation {
    fn prepare_world(
        &mut self,
        request: &Request,
        coupled: Option<&coupled::Request>,
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
        self.coupled = None;
        let mut new = seeded_world(request.settings, domain, request.outputs.surface_meshing)?;
        self.native_roles = roles::NativeRoles::prepare(&mut new, &request.role_setup, domain)?;
        if let Some(coupled) = coupled {
            self.coupled = Some(coupled::Native::prepare(
                &mut new,
                &coupled.setup,
                request.epoch,
                domain,
            )?);
        }
        self.world = Some((request.epoch, new));
        Ok(())
    }

    fn prepare_tick<'request>(
        native_roles: &roles::NativeRoles,
        native: &mut FluidWorld,
        request: &'request Request,
        domain: super::FluidDomainLayout,
        tick: u64,
        sample_time: Seconds,
    ) -> Result<PreparedTick<'request>, String> {
        let interval = request.interval(tick);
        let step = interval.map_or_else(
            || FluidRuntime::step_at(&request.history, tick),
            |interval| FluidRuntime::step_at_interval(&request.history, interval),
        );
        // Every step path below runs after this: plain, impulse-split and
        // coupled frames measure split-hit segments against their parent interval.
        native
            .set_speed_limit_interval(interval.map(StepInterval::duration))
            .map_err(|e| e.to_string())?;
        native
            .set_gravity(step.current.gravity)
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
            interval,
            sample_time,
            domain,
        )?;
        let field = if interval.is_some() {
            FluidRuntime::field_at_time(&request.history, sample_time, domain)
        } else {
            FluidRuntime::field_at(&request.history, tick, domain)
        };
        let begin = request
            .impulses
            .partition_point(|event| event.applied.tick < tick);
        let end = request
            .impulses
            .partition_point(|event| event.applied.tick <= tick);
        let impulse = ImpulseSum {
            events: &request.impulses[begin..end],
            origin: domain.native_origin(),
        };
        Ok(PreparedTick {
            next_obstacle: step.next.obstacle,
            field,
            impulse,
        })
    }

    fn step_plain_live_interval<'request>(
        native: &mut FluidWorld,
        request: &'request Request,
        domain: super::FluidDomainLayout,
        interval: StepInterval,
        prepared: &PreparedTick<'request>,
    ) -> Result<FrameStats, String> {
        let mut frame = native
            .begin_live_frame(interval.duration())
            .map_err(|error| error.to_string())?;
        let events = prepared.impulse.events;
        let mut segment_start = interval.start.0;
        let mut active_events = 0;
        let mut first_active_event = 0;
        let mut frame_remaining = interval.duration().0;
        while segment_start < interval.end.0 {
            while active_events < events.len()
                && events[active_events].source.time.0 <= segment_start
            {
                active_events += 1;
            }
            let segment_end = events.get(active_events).map_or(interval.end.0, |event| {
                event.source.time.0.min(interval.end.0)
            });
            if segment_end <= segment_start {
                break;
            }
            let field =
                FluidRuntime::field_at_time(&request.history, Seconds(segment_start), domain);
            let duration = Seconds(segment_end - segment_start);
            let continuous_fields = [FieldInput {
                field: &field,
                acceleration: 1.0,
                delta_velocity: 0.0,
            }];
            frame
                .set_fields(duration, &continuous_fields)
                .map_err(|error| error.to_string())?;
            let mut impulse_applied = false;
            let mut remaining = if segment_end == interval.end.0 { frame_remaining } else { duration.0 };
            while remaining > 0.0 {
                let offered = frame
                    .next_substep()
                    .map_err(|error| error.to_string())?
                    .ok_or("Fluid live frame ended before its accepted interval")?;
                let step = Seconds(offered.0.min(remaining));
                let impulse = ImpulseSum {
                    events: if impulse_applied {
                        &[]
                    } else {
                        &events[first_active_event..active_events]
                    },
                    origin: domain.native_origin(),
                };
                let fields = [
                    FieldInput {
                        field: &field,
                        acceleration: 1.0,
                        delta_velocity: 0.0,
                    },
                    FieldInput {
                        field: &impulse,
                        acceleration: 0.0,
                        delta_velocity: 1.0,
                    },
                ];
                frame
                    .set_fields(step, &fields)
                    .map_err(|error| error.to_string())?;
                frame.advance(step).map_err(|error| error.to_string())?;
                remaining -= step.0;
                frame_remaining -= step.0;
                impulse_applied = true;
            }
            segment_start = segment_end;
            first_active_event = active_events;
        }
        frame.finish().map_err(|error| error.to_string())
    }

    fn capture_output(
        &mut self,
        request: &mut Request,
        domain: super::FluidDomainLayout,
        tick: u64,
        pose: super::Transform,
        stats: FrameStats,
        coupled: Option<&coupled::Request>,
    ) -> Result<(), String> {
        let last = tick + 1 == request.start_tick + request.count as u64;
        if request.cache_mode != CacheMode::Record && !last {
            return Ok(());
        }
        let native = &mut self.world.as_mut().expect("world initialized").1;
        if request.outputs.surface_meshing {
            native
                .surface(&mut self.surface)
                .map_err(|error| error.to_string())?;
        } else {
            self.surface.clear();
        }
        if self.surface.len() > u32::MAX as usize {
            return Err("Fluid surface exceeds 32-bit GPU vertex indexing".into());
        }
        request
            .recycle
            .try_reserve(self.surface.len())
            .map_err(|error| format!("Fluid surface CPU allocation failed: {error}"))?;
        request
            .recycle
            .extend(self.surface.iter().map(|v| MeshVertex {
                position: domain.to_scene(v.position),
                _pad0: 0.0,
                normal: v.normal,
                _pad1: 0.0,
                uv: [
                    (v.position[0] - (1.5 * domain.cell_size) as f32) / domain.size[0],
                    (v.position[2] - (1.5 * domain.cell_size) as f32) / domain.size[2],
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
            let writer = self.writer.as_ref().expect("record writer initialized");
            if let Some(coupled) = coupled {
                writer.append_paired(
                    tick + 1,
                    &request.recycle,
                    &request.recycle_whitewater,
                    pose,
                    stats,
                    Some(&coupled.output),
                )?;
            } else {
                writer.append(
                    tick + 1,
                    &request.recycle,
                    &request.recycle_whitewater,
                    pose,
                    stats,
                )?;
            }
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
        if request.timing.metadata_only {
            let error = if request.count != 0
                || request.cache_mode != CacheMode::Record
                || self.cache_epoch != Some(request.epoch)
            {
                Some("Physics take: timing update has no initialized recording owner".into())
            } else {
                self.record_input_prefix(&request, 0, request.start_tick, None)
                    .err()
            };
            let mut reply = cancelled_reply(request);
            reply.error = error;
            return reply;
        }
        let mut stats = FrameStats::default();
        let mut pose = request.initial.obstacle;
        let mut setup_error = None;
        let mut completed_count = 0usize;
        let mut playback_tick = None;
        let mut playback_unchanged = false;
        let mut recorded_count = 0usize;
        let mut started_tick = request.start_tick;
        if self.cache_epoch != Some(request.epoch) {
            self.writer = None;
            self.take_writer = None;
            self.world = None;
            self.coupled = None;
            self.playback = None;
            self.cache_epoch = Some(request.epoch);
            if setup_error.is_none() {
                match request.cache_mode {
                    CacheMode::Live => {}
                    CacheMode::Record => {
                        match CacheWriter::create_for_take(
                            request.cache_path.clone(),
                            request.settings,
                        ) {
                            Ok(new_writer) => {
                                self.writer = Some(new_writer);
                                match super::take::Writer::create(
                                    request.cache_path.clone(),
                                    &request,
                                ) {
                                    Ok(writer) => self.take_writer = Some(writer),
                                    Err(error) => setup_error = Some(error),
                                }
                            }
                            Err(error) => setup_error = Some(error),
                        }
                    }
                    CacheMode::Playback => {
                        match CacheReader::open_for_project(
                            request.cache_path.clone(),
                            request.settings,
                            request.project_tempo.as_ref(),
                            request.source_identity,
                            Some(super::PreparedGeometry::from_request(&request)),
                        ) {
                            Ok(reader) => self.playback = Some(reader),
                            Err(error) => setup_error = Some(error),
                        }
                    }
                }
            }
        }
        let mut coupled_request = request.coupled.take();
        if cancel_epoch.load(Ordering::Acquire) != request.epoch {
            request.coupled = coupled_request;
            return cancelled_reply(request);
        }
        let result = (|| -> Result<(), String> {
            if let Some(error) = setup_error {
                return Err(error);
            }
            if request.cache_mode == CacheMode::Playback {
                request.recycle.clear();
                request.recycle_whitewater.clear();
                let cache = self.playback.as_mut().expect("playback initialized");
                let tick = cache.playback_tick(
                    request.playback.map(|playback| playback.address.transport),
                    request
                        .playback
                        .map_or(request.start_tick + request.count as u64, |playback| {
                            playback.address.legacy_tick
                        }),
                )?;
                playback_tick = Some(tick);
                if request
                    .playback
                    .is_some_and(|playback| playback.published_tick == Some(tick))
                {
                    playback_unchanged = true;
                    return Ok(());
                }
                if tick > 0 || coupled_request.is_some() {
                    if let Some(coupled) = &mut coupled_request {
                        let paired;
                        (pose, stats, paired) = cache.read_paired_into(
                            tick,
                            &mut request.recycle,
                            &mut request.recycle_whitewater,
                            Some(&mut coupled.output),
                        )?;
                        if !paired {
                            return Err(
                                "Physics cache: coupled playback requires paired rigid poses"
                                    .into(),
                            );
                        }
                        coupled.output.stamp.epoch = request.epoch;
                    } else {
                        (pose, stats) = cache.read_into(
                            tick,
                            &mut request.recycle,
                            &mut request.recycle_whitewater,
                        )?;
                    }
                    completed_count = request.count;
                    stats.simulation_ms = 0.0;
                    stats.meshing_ms = 0.0;
                }
                return Ok(());
            }
            let domain = request.settings.domain_layout()?;
            let preparing = self.world.is_none();
            self.prepare_world(&request, coupled_request.as_ref(), domain)?;
            if let (Some(native), Some(coupled)) = (&self.coupled, &mut coupled_request) {
                native.prepare_output(&mut coupled.output);
                if preparing || request.count == 0 {
                    native.capture_initial(&mut coupled.output)?;
                }
                if preparing && request.cache_mode == CacheMode::Record {
                    request.recycle_whitewater.clear();
                    self.writer
                        .as_ref()
                        .expect("record writer initialized")
                        .append_paired(
                            0,
                            &[],
                            &request.recycle_whitewater,
                            pose,
                            stats,
                            Some(&coupled.output),
                        )?;
                }
            }
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
                let interval = request.interval(tick);
                if request.schedule.is_some() && interval.is_none() {
                    return Err(format!("Water worker: tick {tick} has no accepted interval"));
                }
                let sample_time = interval.map_or(Seconds(tick as f64 * super::TICK), |interval| {
                    interval.start
                });
                started_tick = tick + 1;
                let (tick_stats, next_obstacle) = {
                    let native = &mut self.world.as_mut().expect("world initialized").1;
                    let prepared = Self::prepare_tick(
                        &self.native_roles,
                        native,
                        &request,
                        domain,
                        tick,
                        sample_time,
                    )?;
                    let tick_stats = if let (Some(rigid), Some(coupled)) =
                        (&mut self.coupled, &mut coupled_request)
                    {
                        let fields = prepared.fields();
                        rigid.step(
                            native,
                            coupled,
                            manifold_physics::TickStamp {
                                epoch: request.epoch,
                                tick,
                            },
                            interval,
                            if prepared.field.is_empty() && prepared.impulse.is_empty() {
                                &[]
                            } else {
                                &fields
                            },
                            prepared.impulse.events,
                            &request.history,
                            domain,
                        )?
                    } else {
                        if prepared
                            .impulse
                            .events
                            .iter()
                            .any(|event| event.value.target.rigid_targets().is_some())
                        {
                            return Err(
                                "Fluid coupling: rigid impulses have no native owner".into()
                            );
                        }
                        let duration =
                            interval.map_or(Seconds(super::TICK), |interval| interval.duration());
                        if let Some(frame_interval) = interval
                            && !prepared.impulse.events.is_empty()
                        {
                            Self::step_plain_live_interval(
                                native,
                                &request,
                                domain,
                                frame_interval,
                                &prepared,
                            )?
                        } else {
                            let fields = prepared.fields();
                            let fields = if prepared.field.is_empty() && prepared.impulse.is_empty()
                            {
                                &[][..]
                            } else {
                                &fields[..]
                            };
                            if interval.is_some() {
                                native.step_live_with_fields(duration, fields)
                            } else if fields.is_empty() {
                                native.step(duration)
                            } else {
                                native.step_with_fields(duration, fields)
                            }
                            .map_err(|e| e.to_string())?
                        }
                    };
                    (tick_stats, prepared.next_obstacle)
                };
                stats = tick_stats;
                completed_count += 1;
                pose = next_obstacle;
                self.capture_output(
                    &mut request,
                    domain,
                    tick,
                    pose,
                    stats,
                    coupled_request.as_ref(),
                )?;
                recorded_count += 1;
            }
            // The particle frame is the batch's last completed tick, or the
            // current tick for a capture-only request.
            let tick = request.start_tick + completed_count as u64;
            let frame_time = request.particle_time(completed_count);
            if let Some(slot) = request.outputs.particles.as_mut()
                && tick > 0
                && cancel_epoch.load(Ordering::Acquire) == request.epoch
            {
                let native = &mut self.world.as_mut().expect("world initialized").1;
                match slot.capture(native, domain.native_origin(), tick, frame_time) {
                    Ok(()) => {}
                    Err(CaptureError::Capacity { particles, solid }) => {
                        request.outputs.growth = Some((particles, solid));
                    }
                    Err(CaptureError::Fluid(error)) => return Err(error.to_string()),
                }
            }
            Ok(())
        })();
        request.coupled = coupled_request;
        let mut error = result.err();
        let accepted_interval = if error.is_none() && completed_count > 0 {
            request.interval(request.start_tick + completed_count as u64 - 1)
        } else { None };
        if self.take_writer.is_some()
            && let Err(record_error) =
                self.record_input_prefix(&request, recorded_count, started_tick, error.as_deref())
        {
            error = Some(match error {
                Some(native_error) => format!("{native_error}; {record_error}"),
                None => record_error,
            });
        }
        Reply {
            outputs: request.outputs,
            source_identity: request.source_identity,
            epoch: request.epoch,
            tick: playback_tick.unwrap_or(request.start_tick + completed_count as u64),
            accepted_interval,
            started_tick,
            impulses: request.impulses,
            history: request.history,
            role_history: request.role_history,
            vertices: request.recycle,
            whitewater: request.recycle_whitewater,
            obstacle: pose,
            stats,
            error,
            coupled: request.coupled,
            timing: request.timing,
            playback: request.playback.map(|playback| super::PlaybackCompletion {
                address: playback.address,
                unchanged: playback_unchanged,
            }),
        }
    }

    fn record_input_prefix(
        &mut self,
        request: &Request,
        completed: usize,
        started_tick: u64,
        failure: Option<&str>,
    ) -> Result<(), String> {
        let take = self
            .take_writer
            .as_mut()
            .ok_or("Physics take: recording has no input journal")?;
        take.append(request, completed, started_tick, failure)?;
        let identity = take.identity();
        self.writer
            .as_mut()
            .ok_or("Physics take: recording has no geometry cache")?
            .publish_take_prefix(identity)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::water::fluid::{TICK, take, take::tests::request};
    use std::sync::Arc;

    #[test]
    fn fluid_take_paired_cache_replays_initial_and_completed_poses_without_native_worlds() {
        paired_playback(false);
    }

    #[test]
    fn fluid_take_paired_cache_uses_project_time_without_native_worlds() {
        paired_playback(true);
    }

    fn paired_playback(timed: bool) {
        let directory = Arc::new(std::env::temp_dir().join(format!(
            "manifold-paired-native-cache-{timed}-{}",
            std::process::id()
        )));
        let mut input = request();
        if timed {
            input.timing.points = [
                (5.0, 0.0),
                (5.0 + 3.0 * TICK, 6.0 * TICK),
                (7.0, 6.0 * TICK),
            ]
            .map(|(transport, simulation)| take::TakeTime {
                beat: manifold_core::Beats(2.0 * transport),
                transport: manifold_core::Seconds(transport),
                simulation: manifold_core::Seconds(simulation),
            })
            .to_vec();
        }
        input.cache_mode = CacheMode::Record;
        input.cache_path = Arc::clone(&directory);
        let expected = NativeSimulation::default().process(input, &AtomicU64::new(1));
        assert_eq!(expected.error, None);
        let mut playback = NativeSimulation::default();
        for tick in [0, 6, 2, 6] {
            let mut reader = take::Reader::open(Arc::clone(&directory)).unwrap();
            let mut input = reader.next_request(23).unwrap().unwrap();
            input.count = tick;
            input.impulses.clear();
            input.cache_mode = CacheMode::Playback;
            input.cache_path = Arc::clone(&directory);
            if timed {
                input.playback = Some(super::super::PlaybackRequest {
                    address: super::super::PlaybackAddress {
                        transport: manifold_core::Seconds(if tick == 6 {
                            7.0
                        } else {
                            5.0 + tick as f64 * TICK / 2.0
                        }),
                        legacy_tick: 1000 + tick as u64,
                    },
                    published_tick: None,
                });
            }
            let actual = playback.process(input, &AtomicU64::new(23));
            assert_eq!(actual.error, None);
            assert_eq!(actual.tick, tick as u64);
            assert!(playback.world.is_none() && playback.coupled.is_none());
            assert_eq!(actual.stats.simulation_ms, 0.0);
            assert_eq!(actual.stats.meshing_ms, 0.0);
            let rigid = actual.coupled.as_ref().unwrap();
            assert_eq!(
                rigid.output.stamp,
                manifold_physics::TickStamp {
                    epoch: 23,
                    tick: tick as u64
                }
            );
            if tick == 0 {
                assert!(actual.vertices.is_empty());
                assert_eq!(
                    rigid.output.poses[0],
                    rigid.setup.initial.bodies[0].as_ref().unwrap().transform
                );
            } else if tick == 6 {
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(&actual.vertices),
                    bytemuck::cast_slice::<_, u8>(&expected.vertices)
                );
                assert_eq!(
                    rigid.output.poses,
                    expected.coupled.as_ref().unwrap().output.poses
                );
                assert_eq!(
                    rigid.output.copies,
                    expected.coupled.as_ref().unwrap().output.copies
                );
            }
        }
        // Each open reconstructs independent Arcs from disk. Equal content
        // above remains valid; changed current assets must fail before a
        // cached liquid surface or rigid pose can be published.
        for change in ["role mesh", "rigid hull", "missing roles", "missing rigid"] {
            let mut reader = take::Reader::open(Arc::clone(&directory)).unwrap();
            let mut input = reader.next_request(24).unwrap().unwrap();
            match change {
                "role mesh" => {
                    let mut value = serde_json::to_value(input.role_setup.as_ref()).unwrap();
                    value["roles"][0]["geometry"]["meshes"][0]["vertices"][0][0] =
                        serde_json::json!(9.0);
                    input.role_setup = Arc::new(serde_json::from_value(value).unwrap());
                }
                "rigid hull" => {
                    let rigid = input.coupled.as_mut().unwrap();
                    let mut value = serde_json::to_value(rigid.setup.as_ref()).unwrap();
                    value["initial"]["bodies"][0]["collider"]["hulls"][0][0][0] =
                        serde_json::json!(9.0);
                    rigid.setup = Arc::new(serde_json::from_value(value).unwrap());
                }
                "missing roles" => input.role_setup = Arc::default(),
                "missing rigid" => input.coupled = None,
                _ => unreachable!(),
            }
            input.cache_mode = CacheMode::Playback;
            input.cache_path = Arc::clone(&directory);
            let mut playback = NativeSimulation::default();
            let actual = playback.process(input, &AtomicU64::new(24));
            assert!(
                actual.error.as_ref().unwrap().contains("geometry"),
                "{change}"
            );
            assert!(actual.vertices.is_empty(), "{change}");
            assert!(playback.world.is_none() && playback.coupled.is_none());
        }
        std::fs::remove_dir_all(directory.as_ref()).unwrap();
    }
}
