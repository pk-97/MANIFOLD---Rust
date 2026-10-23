//! The gate every body passes before the world installs it
//! (SCENE_DOCUMENT_DESIGN.md section 7.8 (Physics and collision shapes)).
//!
//! Each frame's bodies and copy prototypes are checked one by one: pose,
//! scale, contact values and the collision shape at this scale. A body that
//! fails is refused with a reason and the rest of the world keeps stepping.
//! A refused body that was already in the world keeps the last description
//! that passed, so a glitching modulator never restarts the world; one that
//! never passed stays out. A body whose shape is still being prepared has no
//! shape yet and is simply not here; it joins when its shape arrives.

use std::fmt;
use std::sync::Arc;

use manifold_physics::collision_shape::{Solid, validate_hull_scaled, validate_position, validate_scale};

use super::RigidBody;
use super::copy_group::{validate_copy_prototype, validate_spawn};

/// Which world input a refusal is about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Member {
    /// The body on world input `body_k`.
    Body(usize),
    /// The copy group prototype on world input `copies_k`.
    Copies(usize),
}

/// Why a body is out of the world.
#[derive(Clone, Debug, PartialEq)]
pub enum RefusalReason {
    /// The gate refused its pose, scale, contact values or shape.
    Gate(&'static str),
    /// Its body node could not make a collision shape from its mesh.
    Shape(Arc<str>),
    /// It left the world's range while moving and was taken out.
    Lost,
}

impl fmt::Display for RefusalReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gate(reason) => f.write_str(reason),
            Self::Shape(reason) => f.write_str(reason),
            Self::Lost => f.write_str("it moved out of the world's range and was taken out"),
        }
    }
}

/// One body the world left out, and why.
#[derive(Clone, Debug, PartialEq)]
pub struct BodyRefusal {
    pub member: Member,
    pub reason: RefusalReason,
}

#[derive(Default)]
pub(crate) struct Admission {
    bodies: Vec<Option<RigidBody>>,
    groups: Vec<Option<RigidBody>>,
    /// Each slot's last description that passed: the stand-in while its
    /// newest is refused.
    passed_bodies: Vec<Option<RigidBody>>,
    passed_groups: Vec<Option<RigidBody>>,
    refusals: Vec<BodyRefusal>,
    /// The refusals last logged, so a standing refusal logs once.
    logged: Vec<BodyRefusal>,
}

impl Admission {
    /// This frame's admitted bodies and groups, lent out while the world
    /// advances; [`Self::restore_admitted`] hands them back.
    pub(crate) fn take_admitted(&mut self) -> (Vec<Option<RigidBody>>, Vec<Option<RigidBody>>) {
        (std::mem::take(&mut self.bodies), std::mem::take(&mut self.groups))
    }

    pub(crate) fn restore_admitted(&mut self, bodies: Vec<Option<RigidBody>>, groups: Vec<Option<RigidBody>>) {
        self.bodies = bodies;
        self.groups = groups;
    }

    pub(crate) fn refusals(&self) -> &[BodyRefusal] {
        &self.refusals
    }

    /// Forget the bodies taken out after moving out of range: a rebuilt world
    /// starts them again.
    pub(crate) fn clear_lost(&mut self) {
        self.refusals.retain(|refusal| refusal.reason != RefusalReason::Lost);
    }

    /// Record a body the world took out after it moved out of range.
    pub(crate) fn lost(&mut self, member: Member) {
        if !self.refusals.iter().any(|refusal| refusal.member == member) {
            self.refusals.push(BodyRefusal { member, reason: RefusalReason::Lost });
        }
    }

    /// Follow the bodies a recompile moved to other ports: new port `p` takes
    /// what old port `from[p]` held. A refusal for a port that went is
    /// dropped; the rest are logged again under their new ports.
    pub(crate) fn move_ports(&mut self, body_from: &[Option<usize>], group_from: &[Option<usize>]) {
        fn moved(old: &mut [Option<RigidBody>], from: &[Option<usize>]) -> Vec<Option<RigidBody>> {
            from.iter().map(|at| at.and_then(|at| old.get_mut(at).and_then(Option::take))).collect()
        }
        self.bodies = moved(&mut self.bodies, body_from);
        self.groups = moved(&mut self.groups, group_from);
        self.passed_bodies = moved(&mut self.passed_bodies, body_from);
        self.passed_groups = moved(&mut self.passed_groups, group_from);
        let new_port = |member: Member| match member {
            Member::Body(at) => body_from.iter().position(|from| *from == Some(at)).map(Member::Body),
            Member::Copies(at) => group_from.iter().position(|from| *from == Some(at)).map(Member::Copies),
        };
        self.refusals = std::mem::take(&mut self.refusals)
            .into_iter()
            .filter_map(|refusal| new_port(refusal.member).map(|member| BodyRefusal { member, ..refusal }))
            .collect();
        self.logged.clear();
    }

    /// Admit `bodies` and `groups`: what passes, the stand-ins of what was
    /// refused, and nothing for what never passed.
    pub(crate) fn admit(&mut self, bodies: &[Option<RigidBody>], groups: &[Option<RigidBody>]) {
        self.refusals.retain(|refusal| refusal.reason == RefusalReason::Lost);
        for (input, admitted, passed, member, prototype) in [
            (bodies, &mut self.bodies, &mut self.passed_bodies, Member::Body as fn(usize) -> Member, false),
            (groups, &mut self.groups, &mut self.passed_groups, Member::Copies as fn(usize) -> Member, true),
        ] {
            admitted.resize(input.len(), None);
            passed.resize(input.len(), None);
            for (slot, body) in input.iter().enumerate() {
                let Some(body) = body else {
                    admitted[slot] = None;
                    passed[slot] = None;
                    continue;
                };
                match check(body, passed[slot].as_ref(), prototype) {
                    Ok(()) => {
                        passed[slot] = Some(body.clone());
                        admitted[slot] = Some(body.clone());
                    }
                    Err(reason) => {
                        admitted[slot].clone_from(&passed[slot]);
                        if let Some(reason) = reason {
                            self.refusals.push(BodyRefusal { member: member(slot), reason });
                        }
                    }
                }
            }
        }
        if self.refusals != self.logged {
            for refusal in self.refusals.iter().filter(|refusal| !self.logged.contains(refusal)) {
                eprintln!("[physics] {:?} is out of the world: {}", refusal.member, refusal.reason);
            }
            self.logged.clone_from(&self.refusals);
        }
    }
}

/// `Err(None)` while the body's shape is still being prepared: not a
/// refusal, it is just not here yet.
fn check(body: &RigidBody, passed: Option<&RigidBody>, prototype: bool) -> Result<(), Option<RefusalReason>> {
    if !body.enabled {
        return Ok(());
    }
    let gate = |result: Result<(), manifold_physics::PhysicsError>| {
        result.map_err(|error| match error {
            manifold_physics::PhysicsError::InvalidInput(reason) => Some(RefusalReason::Gate(reason)),
            _ => Some(RefusalReason::Gate("Box3D refused the body")),
        })
    };
    let Some(solid) = body.collider.as_deref() else {
        return Err(body.refused.clone().map(RefusalReason::Shape));
    };
    if body.kind > 2 {
        return Err(Some(RefusalReason::Gate("unknown body type")));
    }
    if body.transform.billboard {
        return Err(Some(RefusalReason::Gate("a body cannot turn to face the camera")));
    }
    gate(validate_position(body.transform.pos))?;
    if body.transform.rot_euler.iter().any(|angle| !angle.is_finite()) {
        return Err(Some(RefusalReason::Gate("rotation must be finite")));
    }
    gate(validate_scale(body.transform.scale))?;
    if !body.mass.is_finite() || body.mass < 0.0 || (body.kind == 1 && body.mass == 0.0) {
        return Err(Some(RefusalReason::Gate("mass must be finite and positive")));
    }
    if !body.friction.is_finite() || body.friction < 0.0 || !body.bounce.is_finite() || body.bounce < 0.0 {
        return Err(Some(RefusalReason::Gate("friction and bounce must be finite and not negative")));
    }
    if prototype {
        validate_spawn(body.spawn).map_err(|reason| Some(RefusalReason::Gate(reason)))?;
        validate_copy_prototype(body).map_err(|reason| Some(RefusalReason::Gate(reason)))?;
    }
    // The shape at this scale was checked when this pair last passed.
    let same_shape = passed.is_some_and(|passed| {
        passed.transform.scale == body.transform.scale
            && passed.collider.as_ref().zip(body.collider.as_ref()).is_some_and(|(a, b)| Arc::ptr_eq(a, b))
    });
    if !same_shape {
        let scale = body.transform.scale;
        match solid {
            Solid::Hulls(hulls) => {
                for hull in hulls {
                    gate(validate_hull_scaled(hull, scale))?;
                }
            }
            Solid::Mesh(mesh) => {
                for vertex in &mesh.vertices {
                    gate(validate_position(std::array::from_fn(|axis| vertex[axis] * scale[axis])))?;
                }
            }
        }
    }
    Ok(())
}
