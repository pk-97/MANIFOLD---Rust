//! One copy group of a Physics World: the bodies it installed from its copies
//! body and the arrangement they rest in. Every group shares the world's
//! copy pool of `MAX_COPIES`.

use manifold_node_engine::scene::transform::{quat_to_render_scene_euler, Transform};
use manifold_physics::{BodyConfig, BodyHandle, BodyPose};

use super::copy_layout::{copy_width, Arrangement};
use super::{CopySpawn, DeferredAnimatedEdit, RigidBody};

/// One group's installed copies. `handles` holds every body installed since
/// the world was built, the high-water mark; the first `active` are enabled
/// and the rest wait disabled, so a Count change below the mark never
/// allocates or rebuilds.
#[derive(Default)]
pub(crate) struct CopyGroup {
    pub handles: Vec<Option<BodyHandle>>,
    pub bullet_enabled: Vec<bool>,
    /// The published poses; the first `shown` are drawn.
    pub poses: Vec<Transform>,
    pub description: Option<RigidBody>,
    /// Every copy's hull volume: copies share the prototype's shape and scale.
    pub volume: f32,
    /// One copy's box width per axis, metres, measured when the world is built.
    pub width: [f32; 3],
    pub deferred_edit: Option<DeferredAnimatedEdit>,
    /// The arrangement the world's copies rest in, while the group has bodies.
    pub arrangement: Option<Arrangement>,
    /// Enabled bodies.
    pub active: usize,
    /// Drawn copies: the active bodies, a pending arrangement's preview, or a
    /// disabled group's rest poses.
    pub shown: usize,
}

impl CopyGroup {
    /// Size the group for a fresh world built from `prototype`.
    pub fn reset_for(&mut self, prototype: Option<&RigidBody>) {
        self.handles.clear();
        self.bullet_enabled.clear();
        self.deferred_edit = None;
        self.arrangement = None;
        self.active = 0;
        self.shown = 0;
        self.width = prototype.map_or([1.0; 3], copy_width);
    }

    /// Copy `index`'s rest pose under the installed arrangement.
    pub fn rest(&self, index: usize) -> BodyPose {
        self.arrangement.map_or(BodyPose { position: [0.0; 3], rotation: [0.0, 0.0, 0.0, 1.0] }, |arrangement| {
            arrangement.rest(self.width, index)
        })
    }

    /// Copy `index`'s Box3D config at `pose` with `prototype`'s contact properties.
    pub fn config(&self, prototype: &RigidBody, pose: BodyPose) -> BodyConfig {
        prototype.config_at(pose)
    }

    /// Publish `arrangement`'s first `count` rest poses, growing storage
    /// only past its high-water mark.
    pub fn show_rest(&mut self, arrangement: &Arrangement, scale: [f32; 3]) {
        if self.poses.len() < arrangement.count {
            self.poses.resize(arrangement.count, Transform::default());
        }
        for (index, pose) in self.poses[..arrangement.count].iter_mut().enumerate() {
            let rest = arrangement.rest(self.width, index);
            *pose = Transform {
                pos: rest.position,
                rot_euler: quat_to_render_scene_euler(rest.rotation),
                scale,
                billboard: false,
                ..Default::default()
            };
        }
        self.shown = arrangement.count;
    }
}

pub(crate) fn validate_spawn(spawn: CopySpawn) -> Result<(), String> {
    if !spawn.count.is_finite()
        || !spawn.spacing.is_finite()
        || spawn.spacing <= 0.0
        || !spawn.columns.is_finite()
        || !spawn.layout.is_finite()
        || spawn.turn.iter().any(|turn| !turn.is_finite())
    {
        return Err("Physics: copy count, spacing, columns, layout and turn must be finite; spacing must be positive".into());
    }
    Ok(())
}

pub(crate) fn validate_copy_prototype(prototype: &RigidBody) -> Result<(), String> {
    if prototype.shape >= 5
        || prototype.kind > 2
        || prototype.transform.billboard
        || prototype
            .transform
            .scale
            .iter()
            .any(|s| !s.is_finite() || *s <= 0.0 || *s > 100.0)
    {
        return Err("Physics: copy prototype needs a Platonic shape, positive scale up to 100, and no billboard".into());
    }
    let scale = prototype.transform.scale[0];
    if prototype
        .transform
        .scale
        .iter()
        .any(|axis| (*axis - scale).abs() > 1.0e-5)
    {
        return Err("Physics: copy prototype scale must be uniform".into());
    }
    Ok(())
}
