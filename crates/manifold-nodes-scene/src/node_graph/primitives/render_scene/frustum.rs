//! Whether a mesh's box lies wholly outside a clip volume, so a pass can skip
//! drawing it without changing a pixel.

use manifold_node_engine::scene::mesh_bounds::MeshBounds;

/// Relative slack on every plane test. The GPU transforms a vertex in f32
/// (model first, then the clip matrix), so its clip coordinates differ from
/// the exact ones by a few f32 roundings of the summed term sizes; a box must
/// clear a plane by more than this share of those terms to count as outside.
const SLACK: f64 = 1e-5;

/// True when every corner of `b` (in the mesh's own space, placed by `model`)
/// is outside the same plane of the clip volume `clip` defines. Matrices are
/// column-major `m[col][row]`, the shader layout. The volume is Metal's, for
/// the reversed-Z camera and the forward-Z lights alike: `-w <= x <= w`,
/// `-w <= y <= w`, `0 <= z <= w`.
///
/// Each plane is an affine function of the corner, so a box all on the wrong
/// side of one plane is outside exactly, in front of or behind the eye. A NaN
/// or infinite value makes every comparison it feeds false, so it can never
/// be what culls a box.
pub(super) fn box_outside_clip(
    clip: &[[f32; 4]; 4],
    model: &[[f32; 4]; 4],
    b: &MeshBounds,
) -> bool {
    // m = clip * model; mag holds the same sums of absolute terms.
    let mut m = [[0.0f64; 4]; 4];
    let mut mag = [[0.0f64; 4]; 4];
    for col in 0..4 {
        for row in 0..4 {
            for k in 0..4 {
                let term = f64::from(clip[k][row]) * f64::from(model[col][k]);
                m[col][row] += term;
                mag[col][row] += term.abs();
            }
        }
    }
    // Per plane: still true while every corner so far lies outside it.
    let mut outside = [true; 6];
    for corner in 0..8usize {
        let p: [f64; 3] = std::array::from_fn(|axis| {
            f64::from(if corner >> axis & 1 == 0 {
                b.min[axis]
            } else {
                b.max[axis]
            })
        });
        let mut c = [0.0f64; 4];
        let mut size = [0.0f64; 4];
        for row in 0..4 {
            c[row] = m[3][row];
            size[row] = mag[3][row];
            for axis in 0..3 {
                c[row] += m[axis][row] * p[axis];
                size[row] += mag[axis][row] * p[axis].abs();
            }
        }
        let [x, y, z, w] = c;
        let value = [x + w, w - x, y + w, w - y, z, w - z];
        let scale = [
            size[0] + size[3],
            size[0] + size[3],
            size[1] + size[3],
            size[1] + size[3],
            size[2],
            size[2] + size[3],
        ];
        for plane in 0..6 {
            outside[plane] &= value[plane] < -SLACK * scale[plane];
        }
    }
    outside.contains(&true)
}

#[cfg(test)]
mod tests {
    use super::super::model_matrix;
    use super::*;
    use manifold_node_engine::scene::camera::Camera;
    use manifold_node_engine::scene::light::{Light, ShadowSoftness};

    /// How far past or inside a plane the test boxes sit. The depth planes
    /// get more: a perspective far plane changes clip z so slowly with depth
    /// that the rounding slack spans a few hundredths of a unit there.
    const MARGIN: f64 = 0.05;
    const DEPTH_MARGIN: f64 = 0.5;
    const IDENTITY: [[f32; 4]; 4] = [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];

    /// A clip volume with the geometry the tests aim boxes at: at depth `D`
    /// along `fwd` the volume reaches `slope * D + flat` to each side on the
    /// `right` and `up` axes, and spans depth `near..far`.
    struct Rig {
        clip: [[f32; 4]; 4],
        pos: [f32; 3],
        axes: [[f32; 3]; 3],
        slope: [f64; 2],
        flat: [f64; 2],
        near: f64,
        far: f64,
    }

    impl Rig {
        fn new(
            clip: [[f32; 4]; 4],
            view: [[f32; 4]; 4],
            pos: [f32; 3],
            slope: [f64; 2],
            flat: [f64; 2],
            (near, far): (f32, f32),
        ) -> Self {
            // The view's rows are right, up and backwards.
            let axes = [
                std::array::from_fn(|i| view[i][0]),
                std::array::from_fn(|i| view[i][1]),
                std::array::from_fn(|i| -view[i][2]),
            ];
            Self {
                clip,
                pos,
                axes,
                slope,
                flat,
                near: f64::from(near),
                far: f64::from(far),
            }
        }

        /// A rolled 16:9 reversed-Z perspective camera.
        fn camera() -> Self {
            let cam = Camera::orbit_perspective(0.7, 0.4, 9.0, 0.9, 0.5, 0.6, 0.1, 200.0);
            let aspect = 16.0 / 9.0;
            let tan = f64::from(0.9f32 * 0.5).tan();
            Self::new(
                cam.view_proj(aspect),
                cam.view,
                cam.pos,
                [tan * f64::from(aspect), tan],
                [0.0; 2],
                (cam.near, cam.far),
            )
        }

        /// A sun's orthographic, forward-Z shadow volume.
        fn sun() -> Self {
            let (pos, aim, range) = ([4.0, 12.0, -3.0], [0.0, 0.5, 0.0], 8.0f32);
            let light = Light::sun(
                pos,
                aim,
                [1.0; 3],
                1.0,
                range,
                true,
                ShadowSoftness::Soft,
                0.003,
                1024,
            );
            let d = (0..3)
                .map(|i| (pos[i] - aim[i]).powi(2))
                .sum::<f32>()
                .sqrt();
            Self::new(
                light.shadow_view_proj(),
                light.shadow_view(),
                pos,
                [0.0; 2],
                [f64::from(range); 2],
                (0.0, d + range),
            )
        }

        /// A point light's 90 degree perspective, forward-Z shadow volume.
        fn point() -> Self {
            let (pos, aim, range) = ([-3.0, 5.0, 6.0], [0.0, 0.0, 0.0], 10.0f32);
            let light = Light::point(
                pos,
                aim,
                [1.0; 3],
                1.0,
                range,
                true,
                ShadowSoftness::Soft,
                0.003,
                1024,
            );
            Self::new(
                light.shadow_view_proj(),
                light.shadow_view(),
                pos,
                [1.0; 2],
                [0.0; 2],
                (range * 0.05, range * 4.0),
            )
        }

        /// How far the volume reaches sideways on `axis` at depth `depth`.
        fn reach(&self, axis: usize, depth: f64) -> f64 {
            self.slope[axis] * depth + self.flat[axis]
        }

        /// The model carrying a local box centred on the origin to
        /// (`right`, `up`, `depth`) from the eye, axes parallel to the volume's.
        fn place(&self, centre: [f64; 3]) -> [[f32; 4]; 4] {
            let [right, up, fwd] = self.axes;
            let at: [f32; 3] = std::array::from_fn(|i| {
                (f64::from(self.pos[i])
                    + f64::from(right[i]) * centre[0]
                    + f64::from(up[i]) * centre[1]
                    + f64::from(fwd[i]) * centre[2]) as f32
            });
            [
                [right[0], right[1], right[2], 0.0],
                [up[0], up[1], up[2], 0.0],
                [fwd[0], fwd[1], fwd[2], 0.0],
                [at[0], at[1], at[2], 1.0],
            ]
        }

        fn outside(&self, centre: [f64; 3], half: f64) -> bool {
            let h = half as f32;
            box_outside_clip(
                &self.clip,
                &self.place(centre),
                &MeshBounds {
                    min: [-h; 3],
                    max: [h; 3],
                },
            )
        }
    }

    #[test]
    fn each_plane_culls_just_outside_and_keeps_straddling() {
        for rig in [Rig::camera(), Rig::sun(), Rig::point()] {
            let half = 0.5;
            let depth = rig.near + 0.4 * (rig.far - rig.near);
            assert!(!rig.outside([0.0, 0.0, depth], half), "inside is kept");
            // The nearest sideways corner sits at the deepest of the box's
            // corners, where the volume reaches farthest.
            for axis in 0..2 {
                let edge = half + rig.reach(axis, depth + half);
                for sign in [1.0, -1.0] {
                    let mut centre = [0.0, 0.0, depth];
                    centre[axis] = sign * (edge + MARGIN);
                    assert!(rig.outside(centre, half), "axis {axis} sign {sign} out");
                    centre[axis] = sign * (edge - MARGIN);
                    assert!(!rig.outside(centre, half), "axis {axis} sign {sign} in");
                }
            }
            let (near, far) = (rig.near, rig.far);
            assert!(
                rig.outside([0.0, 0.0, near - half - DEPTH_MARGIN], half),
                "near out"
            );
            assert!(
                !rig.outside([0.0, 0.0, near - half + DEPTH_MARGIN], half),
                "near in"
            );
            assert!(
                rig.outside([0.0, 0.0, far + half + DEPTH_MARGIN], half),
                "far out"
            );
            assert!(
                !rig.outside([0.0, 0.0, far + half - DEPTH_MARGIN], half),
                "far in"
            );
        }
    }

    #[test]
    fn behind_and_around_the_eye_follow_the_planes() {
        for rig in [Rig::camera(), Rig::point()] {
            assert!(
                rig.outside([0.0, 0.0, -10.0], 1.0),
                "wholly behind is past near"
            );
            assert!(
                rig.outside([500.0, 0.0, -10.0], 1.0),
                "behind and off to the side"
            );
            assert!(
                !rig.outside([0.0, 0.0, 0.0], 3.0),
                "a box around the eye is kept"
            );
            // Mostly behind the eye, its front face just past the near plane.
            assert!(!rig.outside([0.0, 0.0, rig.near + DEPTH_MARGIN - 1.0], 1.0));
            // Far wider than the volume, centred in it.
            let mid = rig.near + 0.5 * (rig.far - rig.near);
            assert!(!rig.outside([0.0, 0.0, mid], 1.0e4));
        }
    }

    #[test]
    fn unknown_bounds_are_never_culled() {
        let rig = Rig::camera();
        let model = rig.place([1.0e4, 0.0, -50.0]);
        let far_off = MeshBounds {
            min: [0.0; 3],
            max: [1.0; 3],
        };
        assert!(
            box_outside_clip(&rig.clip, &model, &far_off),
            "control: far off is culled"
        );
        for bad in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            for axis in 0..3 {
                let mut b = far_off;
                b.max[axis] = bad;
                assert!(!box_outside_clip(&rig.clip, &model, &b), "{bad} on {axis}");
                b = far_off;
                b.min[axis] = bad;
                assert!(!box_outside_clip(&rig.clip, &model, &b), "{bad} on {axis}");
            }
        }
        let mut broken = model;
        broken[3][0] = f32::NAN;
        assert!(!box_outside_clip(&rig.clip, &broken, &far_off), "NaN model");
        let mut clip = rig.clip;
        clip[3][3] = f32::NAN;
        assert!(!box_outside_clip(&clip, &model, &far_off), "NaN clip");
    }

    /// The camera's jitter folded into the matrix the way `frame_setup` does,
    /// which is the matrix the draw uniforms carry.
    fn jittered(
        mut view_proj: [[f32; 4]; 4],
        jitter_px: (f32, f32),
        dims: (f32, f32),
    ) -> [[f32; 4]; 4] {
        let wz = view_proj[2][3];
        view_proj[2][0] += (jitter_px.0 * 2.0 / dims.0) * wz;
        view_proj[2][1] += (jitter_px.1 * 2.0 / dims.1) * wz;
        view_proj
    }

    /// With the eye at the origin looking down -z the jitter is exactly a
    /// constant shift of the picture, so a box hugging the right edge flips
    /// with it. The cull must follow the jittered matrix, not the plain one.
    #[test]
    fn the_cull_tests_the_jittered_matrix() {
        let cam = Camera::orbit_perspective(0.7, 0.4, 9.0, 0.9, 0.5, 0.6, 0.1, 200.0);
        let aspect = 16.0 / 9.0;
        let tan = f64::from(0.9f32 * 0.5).tan();
        let rig = Rig::new(
            cam.proj(aspect),
            IDENTITY,
            [0.0; 3],
            [tan * f64::from(aspect), tan],
            [0.0; 2],
            (cam.near, cam.far),
        );
        let (half, depth) = (0.5, 20.0);
        let reach = rig.reach(0, depth + half);
        // The nearest corner 2% of the half width outside, and 2% inside.
        let slack = 0.02 * rig.reach(0, depth);
        let out = rig.place([half + reach + slack, 0.0, depth]);
        let inside = rig.place([half + reach - slack, 0.0, depth]);
        let bounds = MeshBounds {
            min: [-0.5; 3],
            max: [0.5; 3],
        };
        let dims = (64.0, 36.0);
        let plain = rig.clip;
        let left = jittered(plain, (-1.5, 0.0), dims);
        let right = jittered(plain, (1.5, 0.0), dims);
        assert!(box_outside_clip(&plain, &out, &bounds));
        assert!(
            !box_outside_clip(&left, &out, &bounds),
            "a shift of 1.5 px left brings it into view"
        );
        assert!(!box_outside_clip(&plain, &inside, &bounds));
        assert!(
            box_outside_clip(&right, &inside, &bounds),
            "a shift of 1.5 px right pushes it out"
        );
    }

    /// splitmix64: a seeded stream for the property test.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> f64 {
            self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = self.0;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            ((z ^ (z >> 31)) >> 11) as f64 / (1u64 << 53) as f64
        }

        fn range(&mut self, lo: f64, hi: f64) -> f64 {
            lo + (hi - lo) * self.next()
        }
    }

    /// A box with a point that really lands inside the clip volume is never
    /// culled, and the boxes are spread so both outcomes happen often.
    #[test]
    fn a_box_with_a_visible_point_is_never_culled() {
        let mut rng = Rng(0xC0FFEE);
        for rig in [Rig::camera(), Rig::sun()] {
            let (mut culled, mut seen) = (0, 0);
            for _ in 0..2000 {
                let pos = std::array::from_fn(|_| rng.range(-16.0, 16.0) as f32);
                let rot = std::array::from_fn(|_| rng.range(-3.2, 3.2) as f32);
                let scale = std::array::from_fn(|_| rng.range(0.3, 2.5) as f32);
                let model = model_matrix(pos, rot, scale);
                let min: [f32; 3] = std::array::from_fn(|_| rng.range(-3.0, 1.0) as f32);
                let max: [f32; 3] =
                    std::array::from_fn(|axis| min[axis] + rng.range(0.0, 4.0) as f32);
                let bounds = MeshBounds { min, max };
                let is_culled = box_outside_clip(&rig.clip, &model, &bounds);
                culled += usize::from(is_culled);
                // Eight sampled points, each carried through model then clip
                // as the vertex stage does.
                let mut any_inside = false;
                for _ in 0..8 {
                    let local: [f64; 3] = std::array::from_fn(|axis| {
                        rng.range(f64::from(min[axis]), f64::from(max[axis]))
                    });
                    let world: [f64; 4] = std::array::from_fn(|row| {
                        (0..3)
                            .map(|k| f64::from(model[k][row]) * local[k])
                            .sum::<f64>()
                            + f64::from(model[3][row])
                    });
                    let c: [f64; 4] = std::array::from_fn(|row| {
                        (0..4).map(|k| f64::from(rig.clip[k][row]) * world[k]).sum()
                    });
                    any_inside |= c[3] > 0.0
                        && c[0].abs() <= c[3]
                        && c[1].abs() <= c[3]
                        && 0.0 <= c[2]
                        && c[2] <= c[3];
                }
                seen += usize::from(any_inside);
                assert!(
                    !(is_culled && any_inside),
                    "culled a box with a visible point"
                );
            }
            assert!(culled > 300, "too few culled to mean anything: {culled}");
            assert!(seen > 300, "too few visible to mean anything: {seen}");
        }
    }

    #[test]
    fn an_identity_clip_is_the_unit_cube() {
        let b = |lo: f32, hi: f32| MeshBounds {
            min: [lo; 3],
            max: [hi; 3],
        };
        assert!(box_outside_clip(&IDENTITY, &IDENTITY, &b(2.0, 3.0)));
        assert!(!box_outside_clip(&IDENTITY, &IDENTITY, &b(0.5, 3.0)));
        let below = model_matrix([0.0, 0.0, -2.0], [0.0; 3], [1.0; 3]);
        assert!(
            box_outside_clip(&IDENTITY, &below, &b(0.0, 0.5)),
            "z below 0"
        );
    }
}
