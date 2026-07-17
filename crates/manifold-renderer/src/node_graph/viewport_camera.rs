//! `ViewportCamera` — the editor's navigation camera for the P5 3D viewport
//! (`docs/REALTIME_3D_DESIGN.md` D7, P5).
//!
//! This is pure CPU state + math: an orbit-around-target camera with
//! industry-standard navigation (LMB-drag orbit, Shift/MMB-drag pan,
//! scroll/pinch dolly, trackpad two-finger pan) — no GPU, no graph, no
//! `EditingService`. It exists ONLY in the editor preview context (D9): the
//! content thread never constructs one, and nothing here can reach the
//! `Project` or the live show render. [`ViewportCamera::to_camera`] emits the
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

use crate::node_graph::camera::Camera;

/// Radians. Keeps pitch strictly inside ±90° so the analytic frame in
/// `Camera::from_pos_euler` (which this mirrors) never crosses the pole —
/// matches that function's own continuity guarantee.
const MAX_PITCH: f32 = 1.5;
const MIN_DISTANCE: f32 = 0.05;
const MAX_DISTANCE: f32 = 5000.0;

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
            pitch: 0.35,
            distance: 8.0,
            fov_y: 0.9,
            near: 0.05,
            far: 500.0,
        }
    }
}

impl ViewportCamera {
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

    /// Scroll-wheel or trackpad-pinch dolly. `delta` > 0 zooms in (moves
    /// closer); multiplicative so the zoom feels consistent whether the
    /// camera is 1 unit or 1000 units from the target. Clamped so the
    /// camera can never reach the target (a `distance` of 0 would make
    /// `to_camera`'s look-at direction degenerate) or fly to infinity.
    pub fn dolly(&mut self, delta: f32, sensitivity: f32) {
        let factor = (1.0 - delta * sensitivity).clamp(0.1, 10.0);
        self.distance = (self.distance * factor).clamp(MIN_DISTANCE, MAX_DISTANCE);
    }

    /// Trackpad pinch-to-zoom — same math as [`Self::dolly`], separate entry
    /// point for the same per-device-sensitivity reason as
    /// [`Self::trackpad_pan`].
    pub fn trackpad_pinch_dolly(&mut self, delta: f32, sensitivity: f32) {
        self.dolly(delta, sensitivity);
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
    fn trackpad_gestures_apply_same_math_as_mouse() {
        let mut a = ViewportCamera::default();
        let mut b = ViewportCamera::default();
        a.pan(4.0, -2.0, 0.02);
        b.trackpad_pan(4.0, -2.0, 0.02);
        assert_eq!(a.target, b.target);

        let mut c = ViewportCamera::default();
        let mut d = ViewportCamera::default();
        c.dolly(0.5, 0.1);
        d.trackpad_pinch_dolly(0.5, 0.1);
        assert_eq!(c.distance, d.distance);
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
