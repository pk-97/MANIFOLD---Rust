//! One copy group of a Physics World: the bodies it installed from its copies
//! body and the arrangement they rest in. Every group shares the world's
//! copy pool of `MAX_COPIES`.

use std::sync::{Arc, LazyLock};

use manifold_core::copies_body::SPACING_MIN;

use manifold_node_engine::scene::transform::{quat_to_render_scene_euler, Transform};
use manifold_physics::{BodyConfig, BodyHandle, BodyPose, CookedSolid, PhysicsWorld};

use super::copy_layout::{copy_width, requested_count, Arrangement};
use super::{CopySpawn, DeferredAnimatedEdit, RigidBody, Solid, installed_volume, mesh_volume, same_authored_body};

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
    /// The prototype's shape cooked at its scale, which every copy installs.
    cook: Option<CopyCook>,
    /// The prototype the gate refused before it ever passed, drawn at rest.
    refused: Option<RigidBody>,
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
        self.refused = None;
        self.active = 0;
        self.shown = 0;
        self.width = prototype.map_or([1.0; 3], copy_width);
    }

    /// Whether the world has never laid this group out: no description and
    /// no arrangement.
    pub fn is_new(&self) -> bool {
        self.description.is_none() && self.arrangement.is_none()
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
        self.show_rest_from(arrangement, scale, 0);
    }

    /// [`Self::show_rest`] for the copies from `from` on; those before keep
    /// the poses already published.
    pub fn show_rest_from(&mut self, arrangement: &Arrangement, scale: [f32; 3], from: usize) {
        if self.poses.len() < arrangement.count {
            self.poses.resize(arrangement.count, Transform::default());
        }
        for (index, pose) in self.poses[..arrangement.count].iter_mut().enumerate().skip(from) {
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

    /// Draw `prototype`, which the gate never let in, at rest in its layout
    /// as with physics off. Lays out again only when it changes.
    pub fn show_refused(&mut self, prototype: &RigidBody) {
        let count = requested_count(prototype.spawn);
        if self.refused.as_ref().is_some_and(|shown| same_authored_body(shown, prototype)) {
            self.shown = count;
            return;
        }
        self.width = copy_width(prototype);
        self.show_rest(&Arrangement::of(prototype, count), prototype.transform.scale);
        self.refused = Some(prototype.clone());
    }
}

/// A copy group's shape, cooked once: every copy installs it, so a thousand
/// copies cook one hull and a Scale change cooks one more.
struct CopyCook {
    /// The prototype's collider and scale it was cooked from.
    collider: Arc<Solid>,
    scale: [f32; 3],
    cooked: Arc<CookedSolid>,
    /// Whether the group's volume has been measured from it.
    measured: bool,
}

impl CopyGroup {
    /// Install one copy of `prototype` resting at `pose`.
    pub fn install(&mut self, world: &mut PhysicsWorld, prototype: &RigidBody, pose: BodyPose) -> Result<BodyHandle, String> {
        let cooked = self.cooked(prototype)?;
        let handle = world.add_cooked(&cooked, prototype.config_at(pose)).map_err(|error| error.to_string())?;
        self.measure(world, handle, prototype)?;
        Ok(handle)
    }

    /// Give installed copy `handle` the prototype's current shape in place,
    /// keeping its pose and velocity.
    pub fn reshape(&mut self, world: &mut PhysicsWorld, handle: BodyHandle, prototype: &RigidBody, config: BodyConfig) -> Result<(), String> {
        let cooked = self.cooked(prototype)?;
        world.replace_cooked(handle, &cooked, config).map_err(|error| error.to_string())?;
        self.measure(world, handle, prototype)
    }

    /// The prototype's shape at its scale, cooked only when either changed.
    fn cooked(&mut self, prototype: &RigidBody) -> Result<Arc<CookedSolid>, String> {
        let collider = prototype.collider.as_ref().ok_or("the body has no collision shape")?;
        let scale = prototype.transform.scale;
        let current = self.cook.as_ref().is_some_and(|cook| Arc::ptr_eq(&cook.collider, collider) && cook.scale == scale);
        if !current {
            let cooked = collider.cook(scale).map_err(|error| error.to_string())?;
            self.cook = Some(CopyCook { collider: Arc::clone(collider), scale, cooked, measured: false });
        }
        Ok(Arc::clone(&self.cook.as_ref().expect("cooked above").cooked))
    }

    /// How many holders the current cook has: the group and each body made
    /// from it.
    #[cfg(test)]
    pub fn cook_holders(&self) -> usize {
        self.cook.as_ref().map_or(0, |cook| Arc::strong_count(&cook.cooked))
    }

    /// Measure the group's volume from installed copy `handle`, once per cook.
    fn measure(&mut self, world: &PhysicsWorld, handle: BodyHandle, prototype: &RigidBody) -> Result<(), String> {
        let Some(cook) = self.cook.as_mut().filter(|cook| !cook.measured) else {
            return Ok(());
        };
        self.volume = match &*cook.collider {
            Solid::Hulls(_) => installed_volume(world, handle)?,
            Solid::Mesh(mesh) => mesh_volume(mesh, prototype.transform.scale),
        };
        cook.measured = true;
        Ok(())
    }
}

/// The spawner's first value the solver cannot lay copies out with, as the
/// reason and the param it concerns.
pub(crate) fn validate_spawn(spawn: CopySpawn) -> Result<(), (&'static str, &'static str)> {
    if !spawn.count.is_finite() {
        return Err(("Count must be a number", "copy_count"));
    }
    if !spawn.spacing.is_finite() || spawn.spacing < SPACING_MIN {
        return Err((SPACING_REASON.as_str(), "copy_size_spacing"));
    }
    if !spawn.columns.is_finite() {
        return Err(("Columns must be a number", "copy_columns"));
    }
    if !spawn.layout.is_finite() {
        return Err(("Layout must be a number", "copy_layout"));
    }
    if spawn.turn.iter().any(|turn| !turn.is_finite()) {
        return Err(("Turn must be a number", "copy_turn_x"));
    }
    Ok(())
}

/// What the gate says of a Spacing below [`SPACING_MIN`].
static SPACING_REASON: LazyLock<String> = LazyLock::new(|| format!("Spacing must be at least {SPACING_MIN}"));
