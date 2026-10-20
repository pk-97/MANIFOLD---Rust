//! `ViewportCamera` — the editor's navigation camera for the P5 3D viewport
//! (`docs/REALTIME_3D_DESIGN.md` D7, P5).
//!
//! This is pure CPU state + math: an orbit-around-target camera with
//! industry-standard navigation (LMB-drag orbit, Shift/MMB-drag pan,
//! wheel and Ctrl-scroll dolly, trackpad two-finger pan) — no GPU, no graph, no
//! `EditingService`. It exists ONLY in the editor preview context (D9): the
//! UI owns navigation and sends immutable copies to the content renderer.
//! It never edits the `Project` or show camera. [`ViewportCamera::to_camera`] emits the
//! same [`Camera`] struct every 3D consumer already reads, using the
//! `yaw`/`pitch` convention `node.free_camera` uses
//! (`crate::node_graph::primitives::free_camera`) — `viewport_render`
//! substitutes this camera into a render_scene node's `camera` input by
//! injecting a `node.free_camera` node carrying these exact params, so the
//! orbit math here and the position `node.free_camera` computes downstream
//! MUST agree; see that module's doc comment for the splice.
//!
//! Deliberately NOT `Serialize`/`Deserialize` — this is transient editor-session
//! state (per-window navigation), not project data. Restarting the app resets
//! the viewport to its default framing, exactly like every other DCC tool's
//! unsaved viewport camera.

use crate::scene::camera::{Camera, CameraMode};

/// Radians. Keeps pitch strictly inside ±90° so the analytic frame in
/// `Camera::from_pos_euler` (which this mirrors) never crosses the pole —
/// matches that function's own continuity guarantee.
const MAX_PITCH: f32 = 1.5;
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 5000.0;
/// A seeded orbit pivot never sits closer than this in front of the camera.
const MIN_SEED_PIVOT: f32 = 1.0;

/// Orbit-around-target navigation camera. `yaw`/`pitch` follow
/// `node.free_camera`'s convention exactly (yaw about world +Y, pitch about
/// the resulting right axis, yaw=pitch=0 looks down -Z) so
/// `viewport_render::override_camera_def` can hand these fields straight to a
/// synthetic `node.free_camera` node without any basis-to-Euler inverse.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewportCamera {
    /// World-space point the camera orbits and looks at.
    pub target: [f32; 3],
    /// Radians, unbounded (wraps naturally through `sin`/`cos`).
    pub yaw: f32,
    /// Radians, clamped to `(-MAX_PITCH, MAX_PITCH)`.
    pub pitch: f32,
    /// World units from `target` to the camera position.
    pub distance: f32,
    /// Radians (matches `node.free_camera`'s `fov_y` Angle param).
    pub fov_y: f32,
    pub near: f32,
    pub far: f32,
}

impl Default for ViewportCamera {
    /// A three-quarter framing a few units back — sane for "just opened the
    /// viewport on an arbitrary scene" the way every DCC's default view is.
    fn default() -> Self {
        Self {
            target: [0.0, 0.0, 0.0],
            yaw: 0.6,
            // Negative pitch looks down from above the target.
            pitch: -0.35,
            distance: 8.0,
            fov_y: 0.9,
            near: 0.05,
            far: 500.0,
        }
    }
}

impl ViewportCamera {
    /// Navigation that starts exactly where `camera` looks from: same
    /// position, direction and field of view, orbiting the point where its
    /// view ray passes closest to the world origin (at least `MIN_SEED_PIVOT`
    /// ahead). A preview host seeds from the output camera so the first
    /// gesture moves away from the picture on screen instead of jumping.
    /// An orthographic camera keeps the default field of view.
    pub fn seeded_from(camera: &Camera) -> Self {
        let fwd = camera.fwd;
        let pitch = fwd[1].clamp(-1.0, 1.0).asin().clamp(-MAX_PITCH, MAX_PITCH);
        let yaw = (-fwd[0]).atan2(-fwd[2]);
        let ahead = -(camera.pos[0] * fwd[0] + camera.pos[1] * fwd[1] + camera.pos[2] * fwd[2]);
        let distance = ahead.max(MIN_SEED_PIVOT).clamp(MIN_DISTANCE, MAX_DISTANCE);
        let target = [
            camera.pos[0] + fwd[0] * distance,
            camera.pos[1] + fwd[1] * distance,
            camera.pos[2] + fwd[2] * distance,
        ];
        let defaults = Self::default();
        let fov_y = match camera.mode {
            CameraMode::Perspective { fov_y } => fov_y,
            CameraMode::Orthographic { .. } => defaults.fov_y,
        };
        Self { target, yaw, pitch, distance, fov_y, near: camera.near, far: camera.far }
    }

    /// LMB-drag orbit. `dx`/`dy` are screen-pixel deltas; `sensitivity` is
    /// radians per pixel (the panel owns the constant so it can tune per
    /// input device — mouse vs. trackpad drag report different natural
    /// speeds). Positive `dx` (drag right) rotates the view rightward
    /// (yaw increases, matching `free_camera`'s `-yaw.sin()` fwd term so the
    /// apparent motion is "the world turns under the cursor", the standard
    /// orbit feel). Positive `dy` (drag down) looks down.
    pub fn orbit(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.yaw += dx * sensitivity;
        self.pitch = (self.pitch - dy * sensitivity).clamp(-MAX_PITCH, MAX_PITCH);
    }

    /// Shift+LMB-drag or MMB-drag pan. Translates `target` along the
    /// camera's own right/up axes (recomputed from the CURRENT yaw/pitch,
    /// zero roll — the viewport camera never rolls), scaled by `distance` so
    /// pan speed feels constant in screen-space regardless of zoom level
    /// (standard DCC convention: panning a close-up shouldn't fly across the
    /// whole scene). `dx`/`dy` are screen-pixel deltas; `sensitivity` is
    /// world-units-per-pixel-per-unit-distance.
    pub fn pan(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        let (right, up) = self.right_up();
        let scale = sensitivity * self.distance;
        // Drag right moves the CONTENT right under the cursor, i.e. the
        // camera (and its target) move LEFT along `right` — negate.
        for i in 0..3 {
            self.target[i] -= right[i] * dx * scale;
            self.target[i] += up[i] * dy * scale;
        }
    }

    /// Two-finger trackpad pan gesture — same math as [`Self::pan`], separate
    /// entry point so the panel can bind a distinct (typically gentler)
    /// sensitivity constant for trackpad deltas without conflating the two
    /// input devices' natural speeds (D7: "trackpad gestures, refined
    /// later").
    pub fn trackpad_pan(&mut self, dx: f32, dy: f32, sensitivity: f32) {
        self.pan(dx, dy, sensitivity);
    }

    /// Wheel dolly, in notches. `delta` > 0 zooms in (moves
    /// closer). Exponential in `delta`: every unit of delta scales the
    /// distance by the same factor whatever the zoom level, N notches equal
    /// one notch applied N times, and in-then-out returns to the start. A
    /// linear `1 - delta * k` factor goes negative for a large delta and its
    /// clamp turned one wheel notch into a 10x jump. The distance clamp keeps
    /// `to_camera`'s look-at direction defined and the camera finite.
    pub fn dolly(&mut self, delta: f32, sensitivity: f32) {
        let factor = (-delta * sensitivity).exp();
        if factor.is_finite() {
            self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
        }
    }

    /// The camera's right/up basis at the current yaw/pitch, zero roll —
    /// the analytic frame from `Camera::from_pos_euler` (`right0`/`up0`
    /// there), reproduced here so `pan` doesn't need to build a full
    /// `Camera` just to read its basis.
    fn right_up(&self) -> ([f32; 3], [f32; 3]) {
        let right = [self.yaw.cos(), 0.0, -self.yaw.sin()];
        let up = [
            self.yaw.sin() * self.pitch.sin(),
            self.pitch.cos(),
            self.yaw.cos() * self.pitch.sin(),
        ];
        (right, up)
    }

    /// World-space camera position: `target` offset backward along the
    /// `free_camera` fwd direction by `distance` — i.e. `fwd(yaw, pitch)`
    /// points FROM this position TOWARD `target`, exactly the relationship
    /// `node.free_camera`'s own `pos`/`yaw`/`pitch` params must have for the
    /// spliced-in override camera to actually look at the framed scene
    /// (see `viewport_render`).
    pub fn position(&self) -> [f32; 3] {
        let fwd = [
            -self.yaw.sin() * self.pitch.cos(),
            self.pitch.sin(),
            -self.yaw.cos() * self.pitch.cos(),
        ];
        [
            self.target[0] - fwd[0] * self.distance,
            self.target[1] - fwd[1] * self.distance,
            self.target[2] - fwd[2] * self.distance,
        ]
    }

    /// Emit the [`Camera`] this navigation state currently describes, via
    /// `Camera::from_pos_euler` — the SAME builder `node.free_camera` calls,
    /// so a headless test rendering directly through this struct (no graph
    /// splice) is bit-for-bit what the spliced-in `node.free_camera` node
    /// would also produce.
    pub fn to_camera(&self) -> Camera {
        Camera::from_pos_euler(
            self.position(),
            self.yaw,
            self.pitch,
            0.0, // the viewport camera never rolls
            self.fov_y,
            self.near,
            self.far,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_camera_looks_at_target() {
        let vp = ViewportCamera::default();
        let cam = vp.to_camera();
        // fwd should point from pos toward target.
        let to_target = [
            vp.target[0] - cam.pos[0],
            vp.target[1] - cam.pos[1],
            vp.target[2] - cam.pos[2],
        ];
        let len = (to_target[0].powi(2) + to_target[1].powi(2) + to_target[2].powi(2)).sqrt();
        let dir = [to_target[0] / len, to_target[1] / len, to_target[2] / len];
        let dot = dir[0] * cam.fwd[0] + dir[1] * cam.fwd[1] + dir[2] * cam.fwd[2];
        assert!(dot > 0.999, "fwd should point at target, dot={dot}");
    }

    #[test]
    fn position_is_distance_from_target() {
        let vp = ViewportCamera::default();
        let pos = vp.position();
        let d = ((pos[0] - vp.target[0]).powi(2)
            + (pos[1] - vp.target[1]).powi(2)
            + (pos[2] - vp.target[2]).powi(2))
        .sqrt();
        assert!((d - vp.distance).abs() < 1e-4, "expected distance {}, got {d}", vp.distance);
    }

    #[test]
    fn orbit_updates_yaw_and_pitch() {
        let mut vp = ViewportCamera::default();
        let (yaw0, pitch0) = (vp.yaw, vp.pitch);
        vp.orbit(10.0, 5.0, 0.01);
        assert!(vp.yaw > yaw0);
        assert!(vp.pitch < pitch0, "drag down should look down (pitch decreases)");
    }

    #[test]
    fn orbit_clamps_pitch_at_poles() {
        let mut vp = ViewportCamera::default();
        vp.orbit(0.0, 100_000.0, 1.0);
        assert!(vp.pitch >= -MAX_PITCH && vp.pitch <= MAX_PITCH);
        vp.orbit(0.0, -100_000.0, 1.0);
        assert!(vp.pitch >= -MAX_PITCH && vp.pitch <= MAX_PITCH);
    }

    #[test]
    fn dolly_moves_distance_and_clamps() {
        let mut vp = ViewportCamera::default();
        let d0 = vp.distance;
        vp.dolly(1.0, 0.5);
        assert!(vp.distance < d0, "positive delta should zoom in");
        let d1 = vp.distance;
        vp.dolly(-1.0, 0.5);
        assert!(vp.distance > d1, "negative delta should zoom out");

        // Clamp floor.
        for _ in 0..10_000 {
            vp.dolly(1.0, 0.9);
        }
        assert!(vp.distance >= MIN_DISTANCE);
        // Clamp ceiling.
        for _ in 0..10_000 {
            vp.dolly(-1.0, 0.9);
        }
        assert!(vp.distance <= MAX_DISTANCE);
    }

    #[test]
    fn seeded_navigation_reproduces_the_seed_camera() {
        let show = ViewportCamera { target: [1.0, 0.5, -2.0], yaw: -0.8, pitch: 0.3, distance: 6.0, fov_y: 0.7, near: 0.1, far: 300.0 }.to_camera();
        let seeded = ViewportCamera::seeded_from(&show).to_camera();
        for i in 0..3 {
            assert!((seeded.pos[i] - show.pos[i]).abs() < 1e-4, "position {i}");
            assert!((seeded.fwd[i] - show.fwd[i]).abs() < 1e-4, "direction {i}");
        }
        assert_eq!(seeded.mode, show.mode);
        assert_eq!((seeded.near, seeded.far), (show.near, show.far));
    }

    #[test]
    fn dolly_is_bounded_and_proportional_per_notch() {
        let sens = 0.08;
        for notches in [1.0_f32, 2.0, 5.0, 20.0] {
            let mut vp = ViewportCamera::default();
            let d0 = vp.distance;
            vp.dolly(notches, sens);
            let mut step = ViewportCamera::default();
            for _ in 0..notches as usize {
                step.dolly(1.0, sens);
            }
            assert!((vp.distance - step.distance).abs() <= d0 * 1e-4, "{notches} notches at once must equal {notches} single notches");
            let expected = d0 * (-notches * sens).exp();
            assert!((vp.distance - expected).abs() <= d0 * 1e-4);
        }
        let mut one = ViewportCamera::default();
        let d0 = one.distance;
        one.dolly(1.0, sens);
        let ratio = one.distance / d0;
        assert!(ratio > 0.9 && ratio < 0.95, "one notch in moves about 8%, got {ratio}");
        one.dolly(-1.0, sens);
        assert!((one.distance - d0).abs() <= d0 * 1e-5, "in then out returns to the start");
    }

    #[test]
    fn pan_translates_target_without_changing_orientation() {
        let mut vp = ViewportCamera::default();
        let (yaw0, pitch0) = (vp.yaw, vp.pitch);
        let target0 = vp.target;
        vp.pan(5.0, 3.0, 0.01);
        assert_eq!(vp.yaw, yaw0);
        assert_eq!(vp.pitch, pitch0);
        assert_ne!(vp.target, target0);
    }

    #[test]
    fn trackpad_pan_applies_the_same_math_as_mouse_pan() {
        let mut a = ViewportCamera::default();
        let mut b = ViewportCamera::default();
        a.pan(4.0, -2.0, 0.02);
        b.trackpad_pan(4.0, -2.0, 0.02);
        assert_eq!(a.target, b.target);
    }

    #[test]
    fn to_camera_matches_free_camera_builder_exactly() {
        // Cross-check against the exact builder `node.free_camera` calls, so
        // a graph-spliced override camera and a direct `to_camera()` call
        // are provably the same math (viewport_render relies on this).
        let vp = ViewportCamera {
            target: [1.0, 2.0, -3.0],
            yaw: 0.42,
            pitch: -0.2,
            distance: 6.5,
            fov_y: 1.1,
            near: 0.1,
            far: 300.0,
        };
        let cam = vp.to_camera();
        let expected =
            Camera::from_pos_euler(vp.position(), vp.yaw, vp.pitch, 0.0, vp.fov_y, vp.near, vp.far);
        assert_eq!(cam, expected);
    }
}
