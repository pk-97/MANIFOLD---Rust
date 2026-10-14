//! Scene-scale derivation (BUG-upfq scene-param ergonomics): one radius per
//! imported scene, the half bbox diagonal floored at 0.01. Camera framing
//! and the cinematic tail read it here; slider ranges are scaled by the
//! param lookup from the saved `meta.scene_bounds`, never stamped.

use crate::node_graph::primitives::camera_orbit::DEFAULT_FAR as CAMERA_FAR_DEFAULT;

/// The single scene-scale fact imported scenes stamp their slider ranges
/// from. Radius in world units; every derived range width is a multiple of
/// it, so a scene twice as big gets sliders twice as wide.
#[derive(Debug, Clone, Copy)]
pub(super) struct SceneScale {
    /// Floor of the bounding-sphere radius (0.01 — the smallest sensible
    /// feature scale for a unitless model). All derived widths multiply
    /// this, so even a degenerate bbox still yields tappable sliders.
    pub radius: f32,
}

impl SceneScale {
    /// Derive from a `(min, max)` bbox — delegates to the shared
    /// `scene_radius_from_bounds` in manifold-core.
    pub fn from_bbox(min: [f32; 3], max: [f32; 3]) -> Self {
        Self { radius: manifold_core::scene_exposure::scene_radius_from_bounds((min, max)) }
    }
}

/// Camera-framing geometry for the synthesized orbit camera, derived from
/// the import bbox. Computed once at the top of `build_import_graph` and
/// shared by every placement decision below it (camera node params, lens
/// focus seed, sun shadow range, per-light reach). This is the "frame the
/// model" half of scene geometry; [`SceneScale`] is the "size the sliders"
/// half.
#[derive(Debug, Clone, Copy)]
pub(super) struct Framing {
    /// Bbox center (scene context `(0,0,0)`).
    pub center: [f32; 3],
    /// Half the bbox diagonal, floored at `1e-3` — the golden-stable
    /// framing floor. Distinct from [`SceneScale::radius`]'s `0.01`
    /// (BLACK floor for slider widths): small-but-real assets keep their
    /// exact pre-fix framing distances and near/far planes.
    pub radius: f32,
    /// Camera vertical FOV — hoisted here so both the framing-distance fit
    /// and the camera node's own `fov_y` param read the SAME value (a
    /// duplicated literal is how BUG-206-style drift happens).
    pub fov_y: f32,
    /// Framing distance: `2.2 * radius` floored against a per-axis
    /// half-FOV fit, so an elongated asset's dominant axis still fits the
    /// frame with margin.
    pub distance: f32,
    /// Orbit-camera near clip: half the front-face gap (`distance - radius`)
    /// with a `1e-4` floor — never in front of the object (BUG-165/BUG-169),
    /// never so deep it costs depth precision at the object (BUG-774a).
    pub near_clip: f32,
    /// Orbit-camera far clip: `distance + 1.5 * radius` floored at the
    /// camera's DEFAULT_FAR (golden-stable for current assets), capped at
    /// `node.orbit_camera`'s declared range max.
    pub far_clip: f32,
}

impl Framing {
    /// Compute all framing facts from a `(min, max)` bbox. Pure math — no
    /// import state. The `1e-3` radius floor and the `1e-4` front-margin
    /// floor keep degenerate/small assets renderable (BUG-165/BUG-169) and
    /// every currently-passing asset's numbers byte-identical to the
    /// historic import.
    pub fn compute(min: [f32; 3], max: [f32; 3]) -> Self {
        let center = [
            (min[0] + max[0]) * 0.5,
            (min[1] + max[1]) * 0.5,
            (min[2] + max[2]) * 0.5,
        ];
        let dims = [max[0] - min[0], max[1] - min[1], max[2] - min[2]];
        let radius =
            ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
        // `2.2 * radius` alone frames by the bbox half-DIAGONAL,
        // which for an elongated (tall/thin) object barely exceeds its dominant
        // axis — the frame's vertical span contains the object with almost no
        // margin, and camera tilt + perspective push it past the top/bottom
        // edges. Frame by PER-AXIS fit instead: for each axis, the distance
        // required so that half the axis's extent subtends no more than the
        // half-FOV, with a 1.15 safety margin. The render aspect isn't known at
        // import time, so the horizontal half-angle is conservatively treated
        // as equal to the vertical one (square-aspect assumption — never
        // UNDER-frames a wider-than-tall render).
        let fov_y = 0.9_f32;
        let half_fov_tan = (fov_y * 0.5).tan();
        let per_axis_fit = dims
            .iter()
            .map(|&extent| (extent * 0.5) / half_fov_tan * 1.15)
            .fold(0.0f32, f32::max);
        // The `2.2 * radius` floor keeps every COMPACT asset's framing
        // IDENTICAL to before this fix (the golden-stability guarantee:
        // per_axis_fit is only ever larger than the floor for objects
        // dominated by one axis, where the diagonal-based distance genuinely
        // under-frames).
        let distance = (2.2 * radius).max(per_axis_fit);
        // BUG-165/BUG-169 root cause (diagnosed via GLB_XFAIL_BURNDOWN_DESIGN.md
        // P1's `--trace` instrument): `node.orbit_camera`'s `near` default was
        // never scaled to the framed object, so for any object with `radius`
        // below ~0.042 (BoomBox, MetalRoughSpheresNoTextures) the fixed near
        // plane sat IN FRONT of the object and the whole frame clipped to black.
        //
        // Fix: `near` always tracks the object's own front-face distance with a
        // 2x safety margin, in BOTH directions. Down keeps tiny assets rendering
        // (BUG-165/169); up keeps 24-bit depth precision at the object's actual
        // distance — BUG-774a (kuma_heavy_robot: front face ~3300 units out,
        // near pinned at the floor gives ~10 units of depth resolution there,
        // so the model's two overlapping mesh shells z-fight into a triangle
        // mosaic). At `front_margin * 0.5` the front face always gets ~2^23
        // depth slices per unit distance, at any scene scale.
        let front_margin = (distance - radius).max(1e-4);
        let near_clip = front_margin * 0.5;
        // `far` is the same class of bug as `near` above, at the opposite end:
        // the orbit camera's fixed default (CAMERA_FAR_DEFAULT, 200) was never
        // scaled to the framed object either, so any asset whose POSED bbox
        // puts geometry past 200 units depth-clips to a black frame at the
        // default framing (kuma_heavy_robot — 100% black until `far` is raised
        // by hand). The DEFAULT floor keeps every currently-passing asset's far
        // IDENTICAL to before; the 10000 ceiling is `node.orbit_camera`'s
        // declared range max, past which the value would clamp at param load
        // anyway.
        let far_clip = CAMERA_FAR_DEFAULT.max(distance + 1.5 * radius).min(10_000.0);

        Self { center, radius, fov_y, distance, near_clip, far_clip }
    }
}
