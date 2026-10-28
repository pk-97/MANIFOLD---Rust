//! Rigid bodies coupled to a liquid domain: resolved inputs, the visible
//! rigid frame, and the body layout shared by every in-thread liquid.

use manifold_core::scene_impulse::RigidImpulseTargets;
use manifold_node_engine::scene::transform::{Transform, quat_to_render_scene_euler};
use manifold_physics::{BodyHandle, PhysicsWorld, TickStamp};

use crate::physics::{RigidSceneInputs, RigidSimulation};

/// Resolved rigid inputs for one fluid domain. The same rigid world supplies
/// contacts for all its bodies; `colliders` selects those exchanging momentum
/// with this domain. Density is physical liquid density in kg/m³.
#[derive(Clone, Copy)]
pub struct CoupledRigidInputs<'a> {
    pub scene: &'a RigidSceneInputs,
    pub colliders: &'a RigidImpulseTargets,
    pub density: f64,
}

impl CoupledRigidInputs<'_> {
    pub fn validate(self) -> Result<(), String> {
        if !self.density.is_finite() || self.density <= 0.0 {
            return Err("Fluid coupling: density must be finite and positive".into());
        }
        for index in self.colliders.bodies() {
            if self.scene.bodies.get(index).is_none_or(Option::is_none) {
                return Err(format!("Fluid coupling: body {index} is not connected"));
            }
        }
        for group in self.colliders.groups() {
            if self.scene.groups.get(group).is_none_or(Option::is_none) {
                return Err(format!(
                    "Fluid coupling: copy group {group} has no copies body"
                ));
            }
        }
        Ok(())
    }
}

/// An immutable visible rigid result, accepted with the liquid surface at the
/// same completed boundary. Tick zero is the prepared, unstepped scene.
#[derive(Debug)]
pub struct RigidFrame {
    pub stamp: TickStamp,
    /// One per body port.
    pub poses: Vec<Transform>,
    /// Every copy group's copies, group after group.
    pub copies: Vec<Transform>,
    /// How many of `copies` each copy group owns.
    pub group_copies: Vec<usize>,
}

impl Default for RigidFrame {
    fn default() -> Self {
        Self {
            stamp: TickStamp { epoch: 0, tick: 0 },
            poses: Vec::new(),
            copies: Vec::new(),
            group_copies: Vec::new(),
        }
    }
}

impl Clone for RigidFrame {
    fn clone(&self) -> Self {
        Self {
            stamp: self.stamp,
            poses: self.poses.clone(),
            copies: self.copies.clone(),
            group_copies: self.group_copies.clone(),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        self.stamp = source.stamp;
        self.poses.clone_from(&source.poses);
        self.copies.clone_from(&source.copies);
        self.group_copies.clone_from(&source.group_copies);
    }
}

impl RigidFrame {
    /// `a` and `b` blended at `t` (0 is `a`, 1 is `b`): each pose moves
    /// along the straight line between them and turns the short way round.
    /// Frames that hold different bodies or copies take `b` whole.
    pub fn blend_from(&mut self, a: &Self, b: &Self, t: f32) {
        self.clone_from(b);
        if a.poses.len() != b.poses.len() || a.group_copies != b.group_copies {
            return;
        }
        for (shown, (a, b)) in self.poses.iter_mut().chain(self.copies.iter_mut()).zip(
            a.poses
                .iter()
                .chain(&a.copies)
                .zip(b.poses.iter().chain(&b.copies)),
        ) {
            *shown = blend_pose(a, b, t);
        }
    }

    /// Copy group `group`'s copies.
    pub fn group(&self, group: usize) -> &[Transform] {
        let Some(&count) = self.group_copies.get(group) else {
            return &[];
        };
        let start: usize = self.group_copies[..group].iter().sum();
        self.copies.get(start..start + count).unwrap_or(&[])
    }
}

/// A coupled rigid world's bodies as drawn: the frames of its two newest
/// settled ticks with their times, older first, and the frame shown, blended
/// between them at the liquid's display time so the bodies and the liquid
/// they touch move together between ticks.
#[derive(Default)]
pub struct RigidPresentation {
    settled: [Option<(f64, RigidFrame)>; 2],
    shown: RigidFrame,
}

impl RigidPresentation {
    /// Take `frame`, settled at `time`, and draw the bodies at
    /// `display_time`: between the two newest settled ticks, or the newest
    /// whole once the display time reaches it. A new epoch, or time that
    /// went back, starts the pair over.
    pub fn present(&mut self, time: f64, frame: &RigidFrame, display_time: f64) {
        let [older, newer] = &mut self.settled;
        if let Some((_, newest)) = newer.as_mut().filter(|(t, _)| *t == time) {
            // The same tick: bodies may have joined since, so take it again.
            newest.clone_from(frame);
        } else {
            let follows = newer
                .as_ref()
                .is_some_and(|(t, newest)| newest.stamp.epoch == frame.stamp.epoch && *t < time);
            std::mem::swap(older, newer);
            if !follows {
                *older = None;
            }
            // The older pair's storage takes the new tick.
            match newer {
                Some((t, newest)) => {
                    *t = time;
                    newest.clone_from(frame);
                }
                None => *newer = Some((time, frame.clone())),
            }
        }
        match (older.as_ref(), newer.as_ref()) {
            (Some((ta, a)), Some((tb, b))) => {
                let t = ((display_time - ta) / (tb - ta)).clamp(0.0, 1.0);
                self.shown.blend_from(a, b, t as f32);
            }
            (_, Some((_, b))) => self.shown.clone_from(b),
            _ => {}
        }
    }

    /// The frame to draw for the owner's settled `frame`: the blended one
    /// once it holds the same tick and bodies, else `frame` whole.
    pub fn shown<'a>(&'a self, frame: &'a RigidFrame) -> &'a RigidFrame {
        let current = self.settled[1]
            .as_ref()
            .is_some_and(|(_, newest)| newest.stamp == frame.stamp)
            && self.shown.poses.len() == frame.poses.len()
            && self.shown.group_copies == frame.group_copies;
        if current { &self.shown } else { frame }
    }
}

/// `b` with its position and turn blended from `a` at `t`.
fn blend_pose(a: &Transform, b: &Transform, t: f32) -> Transform {
    let (qa, mut qb) = (
        render_scene_euler_to_quat(a.rot_euler),
        render_scene_euler_to_quat(b.rot_euler),
    );
    if (0..4).map(|i| qa[i] * qb[i]).sum::<f32>() < 0.0 {
        qb = qb.map(|v| -v);
    }
    let q: [f32; 4] = std::array::from_fn(|i| qa[i] + (qb[i] - qa[i]) * t);
    let length = q.iter().map(|v| v * v).sum::<f32>().sqrt();
    Transform {
        pos: std::array::from_fn(|i| a.pos[i] + (b.pos[i] - a.pos[i]) * t),
        rot_euler: quat_to_render_scene_euler(q.map(|v| v / length)),
        ..*b
    }
}

/// The quaternion `[x, y, z, w]` of the render scene's Euler triple, the
/// rotation `Rz(z) · Ry(y) · Rx(x)` that [`quat_to_render_scene_euler`]
/// reads back.
fn render_scene_euler_to_quat(e: [f32; 3]) -> [f32; 4] {
    let half = |angle: f32| ((0.5 * angle).sin(), (0.5 * angle).cos());
    let ((sx, cx), (sy, cy), (sz, cz)) = (half(e[0]), half(e[1]), half(e[2]));
    [
        cz * cy * sx - sz * sy * cx,
        cz * sy * cx + sz * cy * sx,
        sz * cy * cx - cz * sy * sx,
        cz * cy * cx + sz * sy * sx,
    ]
}

/// How a prepared rigid world's bodies map onto a [`RigidFrame`]:
/// shared by every liquid that owns a rigid world in-thread.
pub struct CoupledRigidLayout {
    bodies: Vec<Option<BodyHandle>>,
    fragment_parents: Vec<Option<usize>>,
    authored: Vec<Transform>,
    copies: Vec<BodyHandle>,
    group_copies: Vec<usize>,
    group_scales: Vec<[f32; 3]>,
}

impl CoupledRigidLayout {
    /// The layout of `rigid`, prepared from `initial`.
    pub fn new(rigid: &RigidSimulation, initial: &RigidSceneInputs) -> Self {
        let groups = initial.groups.len();
        let mut copies = Vec::new();
        let mut group_copies = Vec::with_capacity(groups);
        for group in 0..groups {
            let before = copies.len();
            copies.extend(rigid.native_copy_handles(group).iter().copied().flatten());
            group_copies.push(copies.len() - before);
        }
        Self {
            bodies: rigid.native_handles().to_vec(),
            fragment_parents: initial
                .bodies
                .iter()
                .map(|body| body.as_ref().and_then(|body| body.fragment_parent))
                .collect(),
            authored: initial
                .bodies
                .iter()
                .map(|body| {
                    body.as_ref()
                        .map_or(Transform::default(), |body| body.transform)
                })
                .collect(),
            copies,
            group_copies,
            group_scales: initial
                .groups
                .iter()
                .map(|body| body.as_ref().map_or([1.0; 3], |body| body.transform.scale))
                .collect(),
        }
    }

    /// Size `output`'s storage for this layout, before a tick captures into it.
    pub fn prepare_output(&self, output: &mut RigidFrame) {
        output.poses.resize(self.bodies.len(), Transform::default());
        output
            .copies
            .resize(self.copies.len(), Transform::default());
        output.group_copies.clone_from(&self.group_copies);
    }

    pub fn capture(&self, world: &PhysicsWorld, output: &mut RigidFrame) -> Result<(), String> {
        if output.poses.len() != self.bodies.len()
            || output.copies.len() != self.copies.len()
            || output.group_copies != self.group_copies
        {
            return Err("Fluid coupling: output storage was not prepared".into());
        }
        for (index, destination) in output.poses.iter_mut().enumerate() {
            let Some(mut handle) = self.bodies[index] else {
                *destination = self.authored[index];
                continue;
            };
            // Prepared, inactive fragment geometry follows its intact parent,
            // exactly as in the ordinary rigid output path.
            if let Some(parent) = self.fragment_parents[index]
                && !world
                    .dynamics(handle)
                    .map_err(|error| error.to_string())?
                    .enabled
            {
                handle = self.bodies[parent].ok_or("Fluid coupling: fragment parent is absent")?;
            }
            let pose = world.pose(handle).map_err(|error| error.to_string())?;
            *destination = Transform {
                pos: pose.position,
                rot_euler: quat_to_render_scene_euler(pose.rotation),
                ..self.authored[index]
            };
        }
        let mut next = 0;
        for (&count, &scale) in self.group_copies.iter().zip(&self.group_scales) {
            for (&handle, destination) in self.copies[next..next + count]
                .iter()
                .zip(&mut output.copies[next..next + count])
            {
                let pose = world.pose(handle).map_err(|error| error.to_string())?;
                *destination = Transform {
                    pos: pose.position,
                    rot_euler: quat_to_render_scene_euler(pose.rotation),
                    scale,
                    billboard: false,
                    ..Default::default()
                };
            }
            next += count;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pose(pos: [f32; 3], rot_euler: [f32; 3]) -> Transform {
        Transform {
            pos,
            rot_euler,
            ..Transform::default()
        }
    }

    #[test]
    fn the_euler_quaternion_reads_back_through_the_render_scene() {
        for e in [[0.3, -0.7, 1.9], [-2.5, 1.2, -0.4], [0.0, 0.0, 3.1]] {
            let back = quat_to_render_scene_euler(render_scene_euler_to_quat(e));
            for axis in 0..3 {
                assert!(
                    (back[axis] - e[axis]).abs() < 1e-4,
                    "{e:?} read back {back:?}"
                );
            }
        }
    }

    #[test]
    fn a_blended_pose_moves_straight_and_turns_the_short_way() {
        let (a, b) = (
            pose([0.0, 1.0, 2.0], [0.0, 0.0, 3.0]),
            pose([2.0, 1.0, -2.0], [0.0, 0.0, -3.0]),
        );
        let half = blend_pose(&a, &b, 0.5);
        assert_eq!(half.pos, [1.0, 1.0, 0.0]);
        // 3 rad and -3 rad about z are 0.28 rad apart through pi, not 6 rad
        // apart through 0.
        let z = half.rot_euler[2];
        assert!(
            (z.abs() - std::f32::consts::PI).abs() < 1e-3,
            "turned through {z}"
        );
        let quarter = blend_pose(&a, &b, 0.25).rot_euler[2];
        assert!(
            (quarter - (3.0 + 0.25 * (2.0 * std::f32::consts::PI - 6.0))).abs() < 1e-3,
            "{quarter}"
        );
        let end = blend_pose(&a, &b, 1.0);
        assert_eq!(end.pos, b.pos);
        assert!((end.rot_euler[2] - b.rot_euler[2]).abs() < 1e-4);
    }

    #[test]
    fn the_presentation_blends_the_two_newest_ticks_and_restarts_on_a_new_epoch() {
        let frame = |epoch, tick, x: f32| RigidFrame {
            stamp: TickStamp { epoch, tick },
            poses: vec![pose([x, 0.0, 0.0], [0.0; 3])],
            ..RigidFrame::default()
        };
        let mut shown = RigidPresentation::default();
        let first = frame(1, 1, 0.0);
        shown.present(1.0 / 30.0, &first, 0.0);
        assert_eq!(shown.shown(&first).poses[0].pos[0], 0.0);
        // Two ticks a thirtieth apart, drawn a quarter and then all the way
        // between them.
        let second = frame(1, 2, 3.0);
        shown.present(2.0 / 30.0, &second, 1.25 / 30.0);
        assert!((shown.shown(&second).poses[0].pos[0] - 0.75).abs() < 1e-4);
        shown.present(2.0 / 30.0, &second, 3.0 / 30.0);
        assert_eq!(shown.shown(&second).poses[0].pos[0], 3.0);
        // A frame the presentation has not taken is drawn whole.
        let unseen = frame(1, 3, 9.0);
        assert_eq!(shown.shown(&unseen).poses[0].pos[0], 9.0);
        // A new epoch never blends with the old one.
        let restarted = frame(2, 1, 5.0);
        shown.present(1.0 / 30.0, &restarted, 0.0);
        assert_eq!(shown.shown(&restarted).poses[0].pos[0], 5.0);
    }

    #[test]
    fn frames_that_hold_different_bodies_show_the_newest_whole() {
        let a = RigidFrame {
            poses: vec![pose([0.0; 3], [0.0; 3])],
            ..RigidFrame::default()
        };
        let b = RigidFrame {
            poses: vec![pose([1.0; 3], [0.0; 3]), pose([5.0; 3], [0.0; 3])],
            ..RigidFrame::default()
        };
        let mut shown = RigidFrame::default();
        shown.blend_from(&a, &b, 0.5);
        assert_eq!(shown.poses, b.poses);
        let c = RigidFrame {
            poses: vec![pose([3.0; 3], [0.0; 3]), pose([7.0; 3], [0.0; 3])],
            ..RigidFrame::default()
        };
        shown.blend_from(&b, &c, 0.5);
        assert_eq!(shown.poses[0].pos, [2.0; 3]);
        assert_eq!(shown.poses[1].pos, [6.0; 3]);
    }
}
