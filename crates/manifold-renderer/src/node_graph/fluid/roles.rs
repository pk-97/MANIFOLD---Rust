//! Prepared scene geometry and bounded, allocation-free live role history.
use std::collections::VecDeque;
use std::sync::Arc;

use manifold_fluids::{FluidWorld, InflowOptions, MeshHandle, MeshRole};
use manifold_physics::input::{input_span, input_span_before};
use manifold_physics::{BodyPose, Seconds, TriangleMesh};

use super::{FluidDomainLayout, HISTORY_CAPACITY, Sample, TICK};
use crate::node_graph::fluid_role::{
    FluidRole, FluidRoleKind, MAX_FLUID_ROLES, PreparedFluidGeometry,
};
use crate::node_graph::physics::pose_from_transform;
use crate::node_graph::transform::Transform;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Controls {
    transform: Transform,
    enabled: bool,
    velocity: [f32; 3],
    inherit_motion: f32,
    friction: f32,
}

impl Controls {
    fn from_role(role: &FluidRole) -> Self {
        Self {
            transform: role.transform,
            enabled: role.enabled,
            velocity: role.velocity,
            inherit_motion: role.inherit_motion,
            friction: role.friction,
        }
    }

    fn interpolate(self, next: Self, alpha: f32) -> Self {
        let lerp = |a: f32, b: f32| a + alpha * (b - a);
        Self {
            transform: Transform {
                pos: std::array::from_fn(|i| lerp(self.transform.pos[i], next.transform.pos[i])),
                rot_euler: std::array::from_fn(|i| {
                    lerp(self.transform.rot_euler[i], next.transform.rot_euler[i])
                }),
                ..self.transform
            },
            velocity: std::array::from_fn(|i| lerp(self.velocity[i], next.velocity[i])),
            inherit_motion: lerp(self.inherit_motion, next.inherit_motion),
            friction: lerp(self.friction, next.friction),
            enabled: if alpha >= 1.0 {
                next.enabled
            } else {
                self.enabled
            },
        }
    }

    fn pose(self, domain: FluidDomainLayout) -> BodyPose {
        let mut pose = pose_from_transform(self.transform);
        pose.position = domain.to_native(pose.position);
        pose
    }
}

struct PreparedRole {
    slot: usize,
    geometry: Arc<PreparedFluidGeometry>,
    kind: FluidRoleKind,
    initial: Controls,
}

#[derive(Default)]
pub(super) struct Setup {
    roles: Vec<PreparedRole>,
}

impl Setup {
    pub fn validate(roles: &[Option<FluidRole>]) -> Result<(), String> {
        if roles.len() > MAX_FLUID_ROLES {
            return Err("Fluid domain has too many connected roles".into());
        }
        for (slot, role) in roles
            .iter()
            .enumerate()
            .filter_map(|(i, role)| role.as_ref().map(|r| (i, r)))
        {
            let transform = role.transform;
            if transform.billboard
                || transform
                    .pos
                    .iter()
                    .chain(&transform.rot_euler)
                    .any(|v| !v.is_finite())
                || transform.scale.iter().any(|v| !v.is_finite() || *v <= 0.0)
                || role.velocity.iter().any(|v| !v.is_finite())
                || !role.inherit_motion.is_finite()
                || role.inherit_motion < 0.0
                || !role.friction.is_finite()
                || !(0.0..=1.0).contains(&role.friction)
                || role.geometry.meshes.is_empty()
            {
                return Err(format!(
                    "Fluid role {slot}: invalid geometry, transform or controls"
                ));
            }
        }
        Ok(())
    }

    pub fn new(roles: &[Option<FluidRole>]) -> Self {
        Self {
            roles: roles
                .iter()
                .enumerate()
                .filter_map(|(slot, role)| {
                    role.as_ref().map(|role| PreparedRole {
                        slot,
                        geometry: Arc::clone(&role.geometry),
                        kind: role.kind,
                        initial: Controls::from_role(role),
                    })
                })
                .collect(),
        }
    }

    pub fn matches(&self, roles: &[Option<FluidRole>]) -> bool {
        self.roles.len() == roles.iter().flatten().count()
            && self.roles.iter().all(|prepared| {
                roles
                    .get(prepared.slot)
                    .and_then(Option::as_ref)
                    .is_some_and(|role| {
                        prepared.kind == role.kind
                            && Arc::ptr_eq(&prepared.geometry, &role.geometry)
                            && prepared.initial.transform.scale == role.transform.scale
                            && (role.kind != FluidRoleKind::InitialFill
                                || prepared.initial == Controls::from_role(role))
                    })
            })
    }

    pub fn len(&self) -> usize {
        self.roles.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::fluid::{FluidControls, FluidRuntime, FluidSettings};
    use manifold_core::Seconds;

    #[test]
    fn scene_physics_domain_role_pose_uses_all_three_origin_axes() {
        let role = role(FluidRoleKind::Inflow);
        let mut controls = Controls::from_role(&role);
        controls.transform.pos = [9.0, -2.0, 4.0];
        controls.transform.rot_euler = [0.0, 0.4, 0.0];
        let domain = FluidSettings {
            domain: Some(Transform {
                pos: [8.0, -3.0, 5.0],
                scale: [6.0, 2.0, 2.0],
                ..Transform::default()
            }),
            ..FluidSettings::default()
        }
        .domain_layout()
        .unwrap();
        let pose = controls.pose(domain);
        assert_eq!(pose.position, [4.0, 2.0, 0.0]);
        let scene_pose = pose_from_transform(controls.transform);
        assert_eq!(pose.rotation, scene_pose.rotation);
    }

    fn role(kind: FluidRoleKind) -> FluidRole {
        FluidRole {
            geometry: Arc::new(PreparedFluidGeometry {
                meshes: vec![
                    manifold_physics::cook_hull_mesh(&[
                        [-0.2, -0.2, -0.2],
                        [0.2, -0.2, -0.2],
                        [-0.2, 0.2, -0.2],
                        [-0.2, -0.2, 0.2],
                    ])
                    .unwrap(),
                ],
            }),
            kind,
            transform: Transform {
                pos: [0.0, 1.0, 0.0],
                ..Transform::default()
            },
            enabled: true,
            velocity: [0.0; 3],
            inherit_motion: 0.0,
            friction: 0.0,
        }
    }

    #[test]
    fn scene_physics_role_history_preserves_live_controls_and_switch_times() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        let mut roles = [None, Some(role(FluidRoleKind::Inflow))];
        runtime
            .observe_scene(settings, controls, &roles, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let setup = Arc::clone(&runtime.role_setup);
        let epoch = runtime.epoch;
        let changed = roles[1].as_mut().unwrap();
        changed.transform.pos[0] = 1.0;
        changed.transform.rot_euler[1] = 1.0;
        changed.velocity[0] = 2.0;
        changed.enabled = false;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.epoch, epoch);
        assert!(Arc::ptr_eq(&runtime.role_setup, &setup));
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        let mut values = Vec::new();
        runtime.role_history.snapshot(&mut values);
        let middle = controls_at(&samples, &values, 1, 0, 0.5);
        assert_eq!(middle.transform.pos[0], 0.5);
        assert_eq!(middle.transform.rot_euler[1], 0.5);
        assert_eq!(middle.velocity[0], 1.0);
        assert!(middle.enabled);
        assert!(!controls_at(&samples, &values, 1, 0, 1.0).enabled);
        let capacity = runtime.role_history.values.capacity();
        let changed = roles[1].as_mut().unwrap();
        changed.velocity[0] = 3.0;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.history.len(), 3);
        assert_eq!(runtime.role_history.values.len(), 3);
        assert_eq!(runtime.role_history.values.back().unwrap().velocity[0], 3.0);
        assert_eq!(runtime.role_history.values.capacity(), capacity);
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        let mut values = Vec::new();
        runtime.role_history.snapshot(&mut values);
        assert_eq!(
            controls_at_before(&samples, &values, 1, 0, 1.0).velocity[0],
            2.0
        );
        assert_eq!(controls_at(&samples, &values, 1, 0, 1.0).velocity[0], 3.0);
    }

    #[test]
    fn scene_physics_role_history_stays_aligned_across_replace_and_prune() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        let mut roles = [
            Some(role(FluidRoleKind::Inflow)),
            None,
            Some(role(FluidRoleKind::Collider)),
        ];
        for role in roles.iter_mut().flatten() {
            role.transform.pos[0] = 0.0;
        }
        runtime
            .observe_scene(settings, controls, &roles, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        roles[0].as_mut().unwrap().transform.pos[0] = 10.0;
        roles[2].as_mut().unwrap().transform.pos[0] = 20.0;
        roles[2].as_mut().unwrap().enabled = false;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        for x in [21.0, 22.0] {
            roles[0].as_mut().unwrap().transform.pos[0] = x;
            roles[2].as_mut().unwrap().transform.pos[0] = 2.0 * x;
            roles[2].as_mut().unwrap().enabled = true;
            runtime
                .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
                .unwrap();
        }
        assert_eq!(runtime.history.len(), 3);
        assert_eq!(runtime.role_history.values.len(), 6);
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        let mut values = Vec::new();
        runtime.role_history.snapshot(&mut values);
        assert_eq!(
            controls_at(&samples, &values, 2, 0, 0.5).transform.pos[0],
            5.0
        );
        assert_eq!(
            controls_at(&samples, &values, 2, 1, 0.5).transform.pos[0],
            10.0
        );
        assert!(!controls_at_before(&samples, &values, 2, 1, 1.0).enabled);
        assert!(controls_at(&samples, &values, 2, 1, 1.0).enabled);

        // Model the consumer completing these samples, without native stepping.
        runtime.completed_tick = 120;
        runtime.prune_history().unwrap();
        assert_eq!(runtime.history.len(), 1);
        assert_eq!(runtime.role_history.values.len(), 2);
        let samples: Vec<_> = runtime.history.iter().cloned().collect();
        runtime.role_history.snapshot(&mut values);
        assert_eq!(
            controls_at(&samples, &values, 2, 0, 1.0).transform.pos[0],
            22.0
        );
        assert_eq!(
            controls_at(&samples, &values, 2, 1, 1.0).transform.pos[0],
            44.0
        );
    }

    #[test]
    fn scene_physics_role_setup_edits_restart_and_remove_old_history() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings::default();
        let controls = FluidControls::default();
        let mut roles = [Some(role(FluidRoleKind::Inflow))];
        runtime
            .observe_scene(settings, controls, &roles, Seconds(0.0), 1.0, 0.0)
            .unwrap();
        let old = runtime.epoch;
        roles[0].as_mut().unwrap().transform.scale[0] = 2.0;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, old);
        assert_eq!(runtime.history.len(), 1);
        assert_eq!(runtime.role_history.values.len(), 1);
        assert_eq!(runtime.target_time, 0.0);
        let scaled = runtime.epoch;
        roles[0].as_mut().unwrap().kind = FluidRoleKind::InitialFill;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, scaled);
        let filled = runtime.epoch;
        roles[0].as_mut().unwrap().transform.pos[1] = 1.5;
        runtime
            .observe_scene(settings, controls, &roles, Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_ne!(runtime.epoch, filled, "initial fill pose is a setup edit");
        runtime
            .observe_scene(settings, controls, &[], Seconds(1.0), 1.0, 0.0)
            .unwrap();
        assert_eq!(runtime.role_setup.len(), 0);
        assert!(runtime.role_history.values.is_empty());
    }

    #[test]
    fn scene_physics_role_runtime_accepts_nonbox_rotated_initial_fill() {
        let mut runtime = FluidRuntime::default();
        let settings = FluidSettings {
            resolution: 12,
            fill_height: 0.0,
            ..FluidSettings::default()
        };
        let controls = FluidControls {
            emission: false,
            obstacle_enabled: false,
            gravity: [0.0; 3],
            ..FluidControls::default()
        };
        let mut source = role(FluidRoleKind::InitialFill);
        source.transform.scale = [3.0; 3];
        source.transform.rot_euler[1] = 0.7;
        let roles = [Some(source)];
        for tick in 0..=2 {
            runtime
                .observe_scene(
                    settings,
                    controls,
                    &roles,
                    Seconds(tick as f64 * TICK),
                    1.0,
                    0.0,
                )
                .unwrap();
            runtime.advance(true).unwrap();
        }
        assert!(runtime.stats.particles > 0);
        assert!(!runtime.vertices.is_empty());
        assert!(
            runtime.vertices.iter().all(|v| v
                .position
                .iter()
                .chain(&v.normal)
                .all(|v| v.is_finite()))
        );
        let positions: Vec<_> = runtime.vertices.iter().map(|v| v.position).collect();
        assert!(
            positions
                .iter()
                .all(|p| p[0].abs() < 1.25 && p[2].abs() < 1.25)
        );
    }

    #[test]
    fn scene_physics_role_closed_channel_preserves_its_cavity() {
        // Extruded U cross-section: one closed, outward-oriented solid with
        // an open channel. A convex hull would occupy the channel itself.
        let outline = [
            [-1.0, -1.0],
            [1.0, -1.0],
            [1.0, 1.0],
            [0.5, 1.0],
            [0.5, -0.5],
            [-0.5, -0.5],
            [-0.5, 1.0],
            [-1.0, 1.0],
        ];
        let vertices: Vec<_> = [-0.75, 0.75]
            .into_iter()
            .flat_map(|z| outline.map(|[x, y]| [x, y, z]))
            .collect();
        let mut triangles = Vec::new();
        for [a, b, c] in [
            [0, 1, 4],
            [0, 4, 5],
            [0, 5, 7],
            [5, 6, 7],
            [1, 2, 3],
            [1, 3, 4],
        ] {
            triangles.extend([[a, c, b], [a + 8, b + 8, c + 8]]);
        }
        for i in 0..8 {
            let j = (i + 1) % 8;
            triangles.extend([[i, j, j + 8], [i, j + 8, i + 8]]);
        }
        let channel = TriangleMesh {
            vertices,
            triangles,
        };
        manifold_fluids::validate_mesh(&channel).unwrap();
        let solid_hull = manifold_physics::cook_hull_mesh(&channel.vertices).unwrap();
        let run = |mesh| {
            let mut collider = role(FluidRoleKind::Collider);
            collider.geometry = Arc::new(PreparedFluidGeometry { meshes: vec![mesh] });
            collider.transform.pos = [0.0, 1.25, 0.0];
            collider.transform.rot_euler[1] = 0.3;
            let mut fill = role(FluidRoleKind::InitialFill);
            let points: Vec<_> = [-0.3, 0.3]
                .into_iter()
                .flat_map(|x| {
                    [-0.3, 0.3]
                        .into_iter()
                        .flat_map(move |y| [-0.3, 0.3].map(|z| [x, y, z]))
                })
                .collect();
            fill.geometry = Arc::new(PreparedFluidGeometry {
                meshes: vec![manifold_physics::cook_hull_mesh(&points).unwrap()],
            });
            fill.transform.pos = [0.0, 1.5, 0.0];
            let settings = FluidSettings {
                resolution: 16,
                fill_height: 0.0,
                ..FluidSettings::default()
            };
            let controls = FluidControls {
                emission: false,
                obstacle_enabled: false,
                gravity: [0.0; 3],
                ..FluidControls::default()
            };
            let scene_roles = [Some(collider), Some(fill)];
            let mut runtime = FluidRuntime::default();
            for tick in 0..=2 {
                runtime
                    .observe_scene(
                        settings,
                        controls,
                        &scene_roles,
                        Seconds(tick as f64 * TICK),
                        1.0,
                        0.0,
                    )
                    .unwrap();
                runtime.advance(true).unwrap();
            }
            assert!(
                runtime
                    .vertices
                    .iter()
                    .all(|v| v.position.iter().all(|p| p.is_finite()))
            );
            runtime.stats.particles
        };
        let inside_channel = run(channel);
        let inside_hull = run(solid_hull);
        assert!(
            inside_channel > 0,
            "closed channel lost its internal liquid"
        );
        assert!(
            inside_channel > inside_hull,
            "channel was treated as a filled hull: {inside_channel} vs {inside_hull}"
        );
    }
}

/// Flat frame-major storage: capacity scales with connected roles, not 64 slots.
#[derive(Default)]
pub(super) struct History {
    values: VecDeque<Controls>,
    stride: usize,
}

impl History {
    pub fn prepare(&mut self, stride: usize) {
        self.clear();
        self.stride = stride;
        self.values.reserve(HISTORY_CAPACITY * stride);
    }
    pub fn clear(&mut self) {
        self.values.clear();
    }
    pub fn latest_matches(&self, setup: &Setup, roles: &[Option<FluidRole>]) -> bool {
        if self.stride != setup.len() || self.values.len() < self.stride {
            return false;
        }
        self.values
            .iter()
            .skip(self.values.len() - self.stride)
            .zip(&setup.roles)
            .all(|(last, prepared)| {
                roles[prepared.slot]
                    .as_ref()
                    .is_some_and(|role| *last == Controls::from_role(role))
            })
    }
    pub fn pop_front(&mut self, count: usize) {
        for _ in 0..count {
            for _ in 0..self.stride {
                self.values.pop_front();
            }
        }
    }
    pub fn observe(&mut self, setup: &Setup, roles: &[Option<FluidRole>], replace: bool) {
        if replace {
            for _ in 0..self.stride {
                self.values.pop_back();
            }
        }
        for prepared in &setup.roles {
            self.values.push_back(Controls::from_role(
                roles[prepared.slot]
                    .as_ref()
                    .expect("validated role topology"),
            ));
        }
    }
    pub fn snapshot(&self, destination: &mut Vec<Controls>) {
        destination.clear();
        // Capacity grows only after topology preparation; ordinary requests reuse it.
        destination.reserve(HISTORY_CAPACITY * self.stride);
        destination.extend(self.values.iter().copied());
    }
}

fn controls_at(
    samples: &[Sample],
    values: &[Controls],
    stride: usize,
    role: usize,
    time: f64,
) -> Controls {
    let span = input_span(samples.iter(), Seconds(time)).expect("observe before role sampling");
    values[span.before_index * stride + role]
        .interpolate(values[span.after_index * stride + role], span.alpha)
}

fn controls_at_before(
    samples: &[Sample],
    values: &[Controls],
    stride: usize,
    role: usize,
    time: f64,
) -> Controls {
    let span =
        input_span_before(samples.iter(), Seconds(time)).expect("observe before role sampling");
    values[span.before_index * stride + role]
        .interpolate(values[span.after_index * stride + role], span.alpha)
}

#[derive(Default)]
pub(super) struct NativeRoles {
    handles: Vec<Vec<MeshHandle>>,
}

impl NativeRoles {
    pub fn prepare(
        world: &mut FluidWorld,
        setup: &Setup,
        domain: FluidDomainLayout,
    ) -> Result<Self, String> {
        let mut result = Self {
            handles: Vec::with_capacity(setup.len()),
        };
        for (index, role) in setup.roles.iter().enumerate() {
            let mut handles = Vec::with_capacity(role.geometry.meshes.len());
            for mesh in &role.geometry.meshes {
                let scaled = TriangleMesh {
                    vertices: mesh
                        .vertices
                        .iter()
                        .map(|p| std::array::from_fn(|i| p[i] * role.initial.transform.scale[i]))
                        .collect(),
                    triangles: mesh.triangles.clone(),
                };
                let initial = role.initial;
                let pose = initial.pose(domain);
                let native = (|| {
                    if role.kind == FluidRoleKind::InitialFill {
                        if initial.enabled {
                            world.add_fluid_mesh(&scaled, pose, initial.velocity)?;
                        }
                    } else {
                        let kind = match role.kind {
                            FluidRoleKind::Inflow => MeshRole::Inflow,
                            FluidRoleKind::Outflow => MeshRole::Outflow,
                            FluidRoleKind::Collider => MeshRole::Collider,
                            FluidRoleKind::InitialFill => unreachable!(),
                        };
                        let handle = world.add_mesh(&scaled, kind, pose)?;
                        world.set_mesh_enabled(handle, initial.enabled)?;
                        if kind == MeshRole::Inflow {
                            world.set_inflow_options(
                                handle,
                                InflowOptions {
                                    velocity: initial.velocity,
                                    inherit_motion: initial.inherit_motion,
                                },
                            )?;
                        } else if kind == MeshRole::Collider {
                            world.set_collider_friction(handle, initial.friction)?;
                        }
                        handles.push(handle);
                    }
                    Ok::<_, manifold_fluids::FluidError>(())
                })();
                native.map_err(|e| format!("Fluid role {index}: {e}"))?;
            }
            result.handles.push(handles);
        }
        Ok(result)
    }

    pub fn apply(
        &self,
        world: &mut FluidWorld,
        setup: &Setup,
        samples: &[Sample],
        values: &[Controls],
        tick: u64,
        domain: FluidDomainLayout,
    ) -> Result<(), String> {
        let current_time = tick as f64 * TICK;
        for (index, role) in setup.roles.iter().enumerate() {
            if role.kind == FluidRoleKind::InitialFill {
                continue;
            }
            let at = |time| controls_at(samples, values, setup.len(), index, time);
            let previous = at((current_time - TICK).max(0.0));
            let current = at(current_time);
            let next = controls_at_before(samples, values, setup.len(), index, current_time + TICK);
            for &handle in &self.handles[index] {
                let native = (|| {
                    world.set_mesh_motion(
                        handle,
                        previous.pose(domain),
                        current.pose(domain),
                        next.pose(domain),
                    )?;
                    world.set_mesh_enabled(handle, current.enabled)?;
                    match role.kind {
                        FluidRoleKind::Inflow => world.set_inflow_options(
                            handle,
                            InflowOptions {
                                velocity: current.velocity,
                                inherit_motion: current.inherit_motion,
                            },
                        )?,
                        FluidRoleKind::Collider => {
                            world.set_collider_friction(handle, current.friction)?
                        }
                        _ => {}
                    }
                    Ok::<_, manifold_fluids::FluidError>(())
                })();
                native.map_err(|e| format!("Fluid role {}: {e}", role.slot))?;
            }
        }
        Ok(())
    }
}
