//! "Use this view" (SCENE_SURFACES D14): the scene camera params that
//! reproduce the editor viewport's view. Pure math; the app turns the
//! result into one undoable command.

use manifold_node_engine::scene::viewport_camera::ViewportCamera;

/// The scene camera kinds a view can be copied into.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ViewCopyCamera {
    Orbit,
    Free,
    /// A Look At camera; `tracked` is the world origin of the item it aims
    /// at (zero without one), which its Target X/Y/Z offset from.
    LookAt {
        tracked: [f32; 3],
    },
}

/// `node.orbit_camera`'s pose params (`camera_orbit.rs`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OrbitParams {
    pub orbit: f32,
    pub tilt: f32,
    pub distance: f32,
    pub look_y: f32,
}

/// Below this squared horizontal length the view ray runs along the
/// vertical axis and has no closest point to it.
const VERTICAL_RAY_EPS: f32 = 1e-8;

/// The orbit pose closest to a view. An orbit camera always looks at
/// `(0, look_y, 0)`, so Target Height is where the view ray passes closest
/// to the vertical axis; a ray along the axis keeps `current_look_y`.
/// Orbit and Tilt invert `camera_orbit.rs`'s eye formula
/// `(d·cos(orbit)·cos(tilt), d·sin(tilt) + look_y, d·sin(orbit)·cos(tilt))`.
pub fn orbit_from_view(eye: [f32; 3], forward: [f32; 3], current_look_y: f32) -> OrbitParams {
    let horizontal = forward[0] * forward[0] + forward[2] * forward[2];
    let look_y = if horizontal < VERTICAL_RAY_EPS {
        current_look_y
    } else {
        let t = -(eye[0] * forward[0] + eye[2] * forward[2]) / horizontal;
        eye[1] + t * forward[1]
    };
    let (dx, dy, dz) = (eye[0], eye[1] - look_y, eye[2]);
    OrbitParams {
        orbit: dz.atan2(dx),
        tilt: dy.atan2((dx * dx + dz * dz).sqrt()),
        distance: (dx * dx + dy * dy + dz * dz).sqrt(),
        look_y,
    }
}

/// `yaw` in `(-π, π]`, the same direction. Bit-exact when already inside.
fn wrap_angle(yaw: f32) -> f32 {
    yaw - std::f32::consts::TAU * (yaw / std::f32::consts::TAU).round()
}

/// The `(param, value)` writes that put a scene camera of `kind` at the
/// viewport's view. Roll is written as 0 because the viewport never rolls.
pub fn camera_writes_from_view(
    kind: ViewCopyCamera,
    view: &ViewportCamera,
    current_look_y: f32,
) -> Vec<(&'static str, f32)> {
    match kind {
        ViewCopyCamera::Orbit => {
            let camera = view.to_camera();
            let p = orbit_from_view(camera.pos, camera.fwd, current_look_y);
            vec![
                ("orbit", p.orbit),
                ("tilt", p.tilt),
                ("distance", p.distance),
                ("look_y", p.look_y),
                ("fov_y", view.fov_y),
                ("roll", 0.0),
            ]
        }
        ViewCopyCamera::Free => {
            let [x, y, z] = view.position();
            vec![
                ("pos_x", x),
                ("pos_y", y),
                ("pos_z", z),
                ("yaw", wrap_angle(view.yaw)),
                ("pitch", view.pitch),
                ("roll", 0.0),
                ("fov_y", view.fov_y),
            ]
        }
        ViewCopyCamera::LookAt { tracked } => {
            let [x, y, z] = view.position();
            let aim = view.target;
            vec![
                ("pos_x", x),
                ("pos_y", y),
                ("pos_z", z),
                ("target_x", aim[0] - tracked[0]),
                ("target_y", aim[1] - tracked[1]),
                ("target_z", aim[2] - tracked[2]),
                ("fov_y", view.fov_y),
            ]
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_node_engine::scene::camera::Camera;

    fn orbit_eye(p: OrbitParams) -> Camera {
        Camera::orbit_perspective(p.orbit, p.tilt, p.distance, 0.9, p.look_y, 0.0, 0.05, 200.0)
    }

    fn assert_close(a: OrbitParams, b: OrbitParams) {
        for (name, x, y) in [
            ("orbit", a.orbit, b.orbit),
            ("tilt", a.tilt, b.tilt),
            ("distance", a.distance, b.distance),
            ("look_y", a.look_y, b.look_y),
        ] {
            assert!((x - y).abs() < 1e-4, "{name}: {x} vs {y}");
        }
    }

    #[test]
    fn orbit_from_view_round_trips_orbit_params() {
        for orbit in [-3.0, -1.2, 0.0, 0.7, 2.5, 3.1] {
            for tilt in [-1.4, -0.5, 0.0, 0.3, 1.2] {
                for (distance, look_y) in [(0.5, 0.0), (4.0, 1.5), (40.0, -3.0)] {
                    let p = OrbitParams {
                        orbit,
                        tilt,
                        distance,
                        look_y,
                    };
                    let cam = orbit_eye(p);
                    assert_close(orbit_from_view(cam.pos, cam.fwd, 99.0), p);
                }
            }
        }
    }

    #[test]
    fn vertical_view_keeps_current_target_height() {
        let p = orbit_from_view([0.0, 5.0, 0.0], [0.0, -1.0, 0.0], 2.0);
        assert_eq!(p.look_y, 2.0);
        assert!((p.distance - 3.0).abs() < 1e-6);
    }

    #[test]
    fn orbit_copy_reproduces_the_viewport_view() {
        let mut view = ViewportCamera {
            target: [0.0, 1.0, 0.0],
            ..Default::default()
        };
        view.orbit(120.0, -40.0, 0.006);
        let writes = camera_writes_from_view(ViewCopyCamera::Orbit, &view, 0.0);
        let get = |name: &str| writes.iter().find(|(p, _)| *p == name).unwrap().1;
        let copied = Camera::orbit_perspective(
            get("orbit"),
            get("tilt"),
            get("distance"),
            get("fov_y"),
            get("look_y"),
            get("roll"),
            0.05,
            200.0,
        );
        let want = view.to_camera();
        for axis in 0..3 {
            assert!(
                (copied.pos[axis] - want.pos[axis]).abs() < 1e-4,
                "pos {axis}"
            );
            assert!(
                (copied.fwd[axis] - want.fwd[axis]).abs() < 1e-4,
                "fwd {axis}"
            );
            assert!((copied.up[axis] - want.up[axis]).abs() < 1e-4, "up {axis}");
        }
        assert_eq!(get("fov_y"), view.fov_y);
    }

    #[test]
    fn free_copy_is_exact() {
        let mut view = ViewportCamera {
            target: [1.5, -0.25, 3.0],
            ..Default::default()
        };
        view.orbit(-75.0, 30.0, 0.006);
        view.dolly(1.0, 0.08);
        let writes = camera_writes_from_view(ViewCopyCamera::Free, &view, 0.0);
        let [x, y, z] = view.position();
        assert_eq!(
            writes,
            vec![
                ("pos_x", x),
                ("pos_y", y),
                ("pos_z", z),
                ("yaw", view.yaw),
                ("pitch", view.pitch),
                ("roll", 0.0),
                ("fov_y", view.fov_y),
            ]
        );
        let get = |name: &str| writes.iter().find(|(p, _)| *p == name).unwrap().1;
        let copied = Camera::from_pos_euler(
            [get("pos_x"), get("pos_y"), get("pos_z")],
            get("yaw"),
            get("pitch"),
            get("roll"),
            get("fov_y"),
            view.near,
            view.far,
        );
        assert_eq!(copied.pos, view.to_camera().pos);
        assert_eq!(copied.fwd, view.to_camera().fwd);
    }

    #[test]
    fn free_copy_wraps_a_wound_up_yaw_to_the_same_direction() {
        let view = ViewportCamera {
            yaw: 0.6 + 3.0 * std::f32::consts::TAU,
            ..Default::default()
        };
        let writes = camera_writes_from_view(ViewCopyCamera::Free, &view, 0.0);
        let yaw = writes.iter().find(|(p, _)| *p == "yaw").unwrap().1;
        assert!(yaw.abs() <= std::f32::consts::PI);
        assert!(
            (yaw.sin() - view.yaw.sin()).abs() < 1e-4 && (yaw.cos() - view.yaw.cos()).abs() < 1e-4
        );
    }
}
