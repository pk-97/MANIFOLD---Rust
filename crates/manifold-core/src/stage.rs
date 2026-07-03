//! Physical stage layout — the data model behind the multi-display / totem
//! canvas. See `docs/MULTI_DISPLAY_DESIGN.md`.
//!
//! `StageLayout` is **authored** data: a list of [`DisplayPlacement`]s the
//! performer arranges on a stage plan. Everything else — island clustering,
//! atlas packing, per-island pixel density — is **derived**, never stored
//! (D3: "Everything is derived, never authored"). [`derive_stage`] is that
//! pure function; [`DerivedStage`] and [`Island`] deliberately do not derive
//! `Serialize`/`Deserialize` so the "never serialized" rule is structural,
//! not just a convention someone can forget.
//!
//! Millimetres throughout except fields explicitly named `_px`.

use serde::{Deserialize, Serialize};

use crate::types::TonemapCurve;

/// Snap tolerance (mm): placements whose post-rotation rects are within this
/// distance of touching merge into one island. See §5.
pub const SNAP_TOLERANCE_MM: f32 = 5.0;

/// Gutter (px) inserted between packed islands in the render atlas, so a
/// neighborhood GPU op (blur, convolution) on one island's edge never samples
/// into an unrelated island. See §3 D2.
pub const ATLAS_GUTTER_PX: u32 = 16;

// ─── OutputId ───

/// Stable, project-scoped identifier for a display/output placement. Minted
/// once when a placement is created and never reused within a project.
///
/// A `u64` counter rather than the `Arc<str>` UUID pattern used for
/// `ClipId`/`LayerId`/etc. (`manifold_foundation::id`): placements are
/// rig-sized (a handful, not content-sized), never need cross-project
/// stability on their own (a venue import re-keys — see
/// `manifold-io::venue_file`), and a dense counter makes deterministic
/// ordering in tests trivial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OutputId(pub u64);

impl OutputId {
    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

// ─── Rotation ───

/// Per-placement rotation. Vertical totems are usually landscape panels
/// rotated 90/270; rotation applies in the output blit (§6.2), never in
/// content (§6.1 — zero shader changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    /// Whether this rotation swaps the panel's width/height on the stage plan.
    pub fn swaps_dimensions(self) -> bool {
        matches!(self, Rotation::R90 | Rotation::R270)
    }
}

// ─── DisplayIdentity ───

/// Stable identity for re-matching a placement to a live physical display
/// across launches/reboots (`CGDirectDisplayID`s are not stable). Match on
/// `uuid` first, then `name`; `None` on both = unassigned. See §5
/// "Display identity rules (gig-critical)".
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DisplayIdentity {
    /// `CGDisplayCreateUUIDFromDisplayID` — the stable match key.
    pub uuid: Option<String>,
    /// `NSScreen.localizedName` — fallback match + human label.
    pub name: String,
}

// ─── OutputAdvanced ───

/// Per-output advanced calibration — the "advanced flap," closed by default
/// in the stage view (§5). Output-transform-only (§6.2, §7.4): none of these
/// fields change content, only how an island's pixels reach the device.
///
/// `density_cap_px_per_mm` is the only field `derive_stage` reads in P1; the
/// rest (keystone, color trim, tonemap override) are consumed by the P3
/// per-output present pass and land wired-up with the P5 stage view UI — this
/// struct just carries their storage now so `DisplayPlacement`'s shape is
/// final from P1 onward.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OutputAdvanced {
    /// 4-corner keystone homography in normalized device coordinates
    /// (top-left, top-right, bottom-right, bottom-left). `None` = identity —
    /// covers a flat, square-ish projector throw (§5, §7.4).
    pub keystone_corners: Option<[[f32; 2]; 4]>,
    /// Multiplicative RGB gain trim, applied at present time.
    pub color_gain: [f32; 3],
    /// Additive RGB lift trim, applied at present time.
    pub color_lift: [f32; 3],
    /// Per-output tonemap curve override. `None` = project default
    /// (`ProjectSettings::tonemap_curve`).
    pub tonemap_override: Option<TonemapCurve>,
    /// Cap this display's contribution to its island's render density
    /// (px/mm) — for LED processors that report absurd native modes.
    /// `None` = native density (§9 "Density knobs").
    pub density_cap_px_per_mm: Option<f32>,
}

impl Default for OutputAdvanced {
    fn default() -> Self {
        Self {
            keystone_corners: None,
            color_gain: [1.0, 1.0, 1.0],
            color_lift: [0.0, 0.0, 0.0],
            tonemap_override: None,
            density_cap_px_per_mm: None,
        }
    }
}

// ─── DisplayPlacement ───

/// One physical output (display, projector, LED processor) positioned on the
/// stage plan. `physical_size_mm` and `native_resolution` are pre-rotation
/// (the panel's own dimensions); `position_mm` is post-rotation (top-left of
/// the placement's actual stage-plan footprint). See §5.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DisplayPlacement {
    pub id: OutputId,
    #[serde(default)]
    pub name: String,
    /// Pre-rotation panel size (mm). For a projector this is the measured
    /// throw size, not a physical device dimension (§7.4).
    pub physical_size_mm: [f32; 2],
    /// Pre-rotation pixel mode MANIFOLD drives this output at.
    pub native_resolution: [u32; 2],
    /// Top-left of this placement's post-rotation footprint on the stage plan (mm).
    pub position_mm: [f32; 2],
    #[serde(default)]
    pub rotation: Rotation,
    /// Which live physical monitor this placement matches. `None` = unassigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<DisplayIdentity>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub advanced: OutputAdvanced,
}

fn default_enabled() -> bool {
    true
}

impl DisplayPlacement {
    /// Footprint size on the stage plan, post-rotation (mm).
    pub fn footprint_mm(&self) -> [f32; 2] {
        if self.rotation.swaps_dimensions() {
            [self.physical_size_mm[1], self.physical_size_mm[0]]
        } else {
            self.physical_size_mm
        }
    }

    /// Stage-plan rect: `[x, y, w, h]` mm, post-rotation.
    pub fn stage_rect_mm(&self) -> [f32; 4] {
        let [w, h] = self.footprint_mm();
        [self.position_mm[0], self.position_mm[1], w, h]
    }

    /// Native pixel density (px/mm), pre-rotation and rotation-invariant
    /// (rotating a panel doesn't change how many pixels it packs per
    /// millimetre). Computed from the width axis — MANIFOLD assumes square
    /// pixels, true of every panel/projector mode in practice.
    pub fn native_density_px_per_mm(&self) -> f32 {
        if self.physical_size_mm[0] <= 0.0 {
            return 0.0;
        }
        self.native_resolution[0] as f32 / self.physical_size_mm[0]
    }

    /// Density after the advanced-flap cap (§9): `min(native, cap)`.
    pub fn effective_density_px_per_mm(&self) -> f32 {
        let native = self.native_density_px_per_mm();
        match self.advanced.density_cap_px_per_mm {
            Some(cap) if cap > 0.0 => native.min(cap),
            _ => native,
        }
    }
}

// ─── StageLayout ───

/// Physical arrangement of outputs on the stage plan. Serialized inside
/// `ProjectSettings` (§5) — the single source of truth at runtime — and
/// separately exportable/importable as a standalone venue file
/// (`manifold-io::venue_file`, D13).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StageLayout {
    pub placements: Vec<DisplayPlacement>,
}

impl StageLayout {
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// Mint the next unused id in this layout (one past the current max, or 0
    /// for the first placement).
    pub fn next_output_id(&self) -> OutputId {
        OutputId(
            self.placements
                .iter()
                .map(|p| p.id.0)
                .max()
                .map_or(0, |m| m + 1),
        )
    }

    pub fn find(&self, id: OutputId) -> Option<&DisplayPlacement> {
        self.placements.iter().find(|p| p.id == id)
    }

    pub fn find_mut(&mut self, id: OutputId) -> Option<&mut DisplayPlacement> {
        self.placements.iter_mut().find(|p| p.id == id)
    }
}

// ─── Derived stage (never serialized — D3) ───

/// One contiguous pixel region — a cluster of abutting placements, or a
/// single placement with no neighbours. Always derived by [`derive_stage`],
/// never authored or stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Island {
    pub display_ids: Vec<OutputId>,
    /// Bounding rect on the stage plan (mm): `[x, y, w, h]`.
    pub stage_rect_mm: [f32; 4],
    /// This island's uniform render density (px/mm) — the densest member's
    /// effective (capped) density.
    pub px_per_mm: f32,
    /// This island's region within the packed atlas (px): `[x, y, w, h]`.
    pub atlas_region_px: [u32; 4],
}

/// The fully-derived stage: islands + the atlas size that contains them.
/// An empty `islands` list (from an empty [`StageLayout`]) is the signal a
/// renderer uses to take the legacy single-canvas path
/// (`ProjectSettings::output_width/height`) instead of the atlas — that
/// fallback is a P2 rendering concern, not something this data type encodes,
/// per `derive_stage`'s committed `&StageLayout -> DerivedStage` signature
/// (no access to `ProjectSettings` dimensions).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DerivedStage {
    pub islands: Vec<Island>,
    pub atlas_size: [u32; 2],
}

/// Derive islands, packing, and density from a stage layout. Pure function,
/// no allocation surprises for the hot path: callers re-derive per-action
/// (on a stage edit), never per-frame (§9).
///
/// Disabled placements (`enabled: false`) are excluded entirely — they
/// contribute no pixels and cannot merge into an island.
pub fn derive_stage(layout: &StageLayout) -> DerivedStage {
    let placements: Vec<&DisplayPlacement> =
        layout.placements.iter().filter(|p| p.enabled).collect();

    if placements.is_empty() {
        return DerivedStage::default();
    }

    let mut islands: Vec<Island> = cluster_placements(&placements)
        .into_iter()
        .map(|members| {
            let stage_rect_mm = bounding_rect_mm(&members);
            let px_per_mm = members
                .iter()
                .map(|p| p.effective_density_px_per_mm())
                .fold(0.0f32, f32::max);
            let mut display_ids: Vec<OutputId> = members.iter().map(|p| p.id).collect();
            display_ids.sort();
            Island {
                display_ids,
                stage_rect_mm,
                px_per_mm,
                atlas_region_px: [0, 0, 0, 0],
            }
        })
        .collect();

    // Deterministic order: by each island's lowest member id.
    islands.sort_by_key(|isl| isl.display_ids[0]);

    // Pack left-to-right in a single row with a gutter between islands. The
    // packing *algorithm* isn't committed by the design doc (an interior
    // detail — islands are few, rig-sized); this is the simplest layout that
    // satisfies D2 (no pixels allocated for the gap between islands).
    let mut cursor_x = 0u32;
    let mut atlas_h = 0u32;
    for island in &mut islands {
        let w_px = (island.stage_rect_mm[2] * island.px_per_mm).round().max(1.0) as u32;
        let h_px = (island.stage_rect_mm[3] * island.px_per_mm).round().max(1.0) as u32;
        island.atlas_region_px = [cursor_x, 0, w_px, h_px];
        atlas_h = atlas_h.max(h_px);
        cursor_x += w_px + ATLAS_GUTTER_PX;
    }
    let atlas_w = cursor_x.saturating_sub(ATLAS_GUTTER_PX);

    DerivedStage {
        islands,
        atlas_size: [atlas_w, atlas_h],
    }
}

/// Two rects (as `[x, y, w, h]`) are within `tol` of touching if their gap is
/// `<= tol` on both axes (a negative gap means they already overlap on that
/// axis). Standard AABB expand-and-test distance check.
fn rects_touch(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    let (ax0, ay0, ax1, ay1) = (a[0], a[1], a[0] + a[2], a[1] + a[3]);
    let (bx0, by0, bx1, by1) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);

    let x_gap = (bx0 - ax1).max(ax0 - bx1);
    let y_gap = (by0 - ay1).max(ay0 - by1);
    x_gap <= tol && y_gap <= tol
}

/// Union-find clustering of placements whose post-rotation stage rects touch
/// within [`SNAP_TOLERANCE_MM`].
fn cluster_placements<'a>(
    placements: &[&'a DisplayPlacement],
) -> Vec<Vec<&'a DisplayPlacement>> {
    let n = placements.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], i: usize) -> usize {
        if parent[i] != i {
            parent[i] = find(parent, parent[i]);
        }
        parent[i]
    }

    let rects: Vec<[f32; 4]> = placements.iter().map(|p| p.stage_rect_mm()).collect();
    for i in 0..n {
        for j in (i + 1)..n {
            if rects_touch(rects[i], rects[j], SNAP_TOLERANCE_MM) {
                let ri = find(&mut parent, i);
                let rj = find(&mut parent, j);
                if ri != rj {
                    parent[ri] = rj;
                }
            }
        }
    }

    let mut groups: std::collections::BTreeMap<usize, Vec<&'a DisplayPlacement>> =
        std::collections::BTreeMap::new();
    for (i, &p) in placements.iter().enumerate() {
        let root = find(&mut parent, i);
        groups.entry(root).or_default().push(p);
    }
    groups.into_values().collect()
}

fn bounding_rect_mm(members: &[&DisplayPlacement]) -> [f32; 4] {
    let mut x0 = f32::MAX;
    let mut y0 = f32::MAX;
    let mut x1 = f32::MIN;
    let mut y1 = f32::MIN;
    for p in members {
        let r = p.stage_rect_mm();
        x0 = x0.min(r[0]);
        y0 = y0.min(r[1]);
        x1 = x1.max(r[0] + r[2]);
        y1 = y1.max(r[1] + r[3]);
    }
    [x0, y0, x1 - x0, y1 - y0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn placement(id: u64, pos: [f32; 2], size: [f32; 2], native: [u32; 2]) -> DisplayPlacement {
        DisplayPlacement {
            id: OutputId(id),
            name: format!("Output {id}"),
            physical_size_mm: size,
            native_resolution: native,
            position_mm: pos,
            rotation: Rotation::R0,
            identity: None,
            enabled: true,
            advanced: OutputAdvanced::default(),
        }
    }

    // ── Empty layout = legacy single island ──

    #[test]
    fn empty_layout_derives_no_islands() {
        let layout = StageLayout::default();
        let derived = derive_stage(&layout);
        assert!(
            derived.islands.is_empty(),
            "empty StageLayout must signal 'no atlas' so the renderer takes the \
             legacy single-canvas path (ProjectSettings::output_width/height)"
        );
        assert_eq!(derived.atlas_size, [0, 0]);
    }

    #[test]
    fn disabled_only_layout_derives_no_islands() {
        let mut p = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        p.enabled = false;
        let layout = StageLayout {
            placements: vec![p],
        };
        assert!(derive_stage(&layout).islands.is_empty());
    }

    // ── Clustering / snap ──

    #[test]
    fn abutting_placements_merge_into_one_island() {
        // Two 500mm-wide panels placed edge-to-edge (touching exactly).
        let a = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let b = placement(1, [500.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let layout = StageLayout {
            placements: vec![a, b],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands.len(), 1, "abutting panels form one island");
        assert_eq!(derived.islands[0].display_ids, vec![OutputId(0), OutputId(1)]);
        assert_eq!(derived.islands[0].stage_rect_mm, [0.0, 0.0, 1000.0, 1000.0]);
    }

    #[test]
    fn placements_within_snap_tolerance_merge() {
        // 3mm gap, under the 5mm snap tolerance.
        let a = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let b = placement(1, [503.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let layout = StageLayout {
            placements: vec![a, b],
        };
        assert_eq!(derive_stage(&layout).islands.len(), 1);
    }

    #[test]
    fn placements_beyond_snap_tolerance_stay_separate_islands() {
        // Two totems 3 metres apart — the driving use case (§0).
        let a = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let b = placement(1, [3500.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let layout = StageLayout {
            placements: vec![a, b],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands.len(), 2, "a real gap must not merge");
        // No pixels allocated for the 3m gap: atlas width is the sum of the
        // two islands' pixel widths plus one gutter, nothing more.
        let expected_w = derived.islands[0].atlas_region_px[2]
            + ATLAS_GUTTER_PX
            + derived.islands[1].atlas_region_px[2];
        assert_eq!(derived.atlas_size[0], expected_w);
    }

    #[test]
    fn three_by_two_wall_merges_into_one_island() {
        // A 3x2 grid of 500x500mm panels, all abutting — one island.
        let mut placements = Vec::new();
        let mut id = 0u64;
        for row in 0..2 {
            for col in 0..3 {
                placements.push(placement(
                    id,
                    [col as f32 * 500.0, row as f32 * 500.0],
                    [500.0, 500.0],
                    [1080, 1080],
                ));
                id += 1;
            }
        }
        let layout = StageLayout { placements };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands.len(), 1, "a panel wall is one island");
        assert_eq!(derived.islands[0].display_ids.len(), 6);
        assert_eq!(derived.islands[0].stage_rect_mm, [0.0, 0.0, 1500.0, 1000.0]);
    }

    // ── Rotation ──

    #[test]
    fn rotation_swaps_footprint_dimensions() {
        let mut p = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        assert_eq!(p.footprint_mm(), [500.0, 1000.0]);
        p.rotation = Rotation::R90;
        assert_eq!(p.footprint_mm(), [1000.0, 500.0]);
        p.rotation = Rotation::R270;
        assert_eq!(p.footprint_mm(), [1000.0, 500.0]);
        p.rotation = Rotation::R180;
        assert_eq!(p.footprint_mm(), [500.0, 1000.0]);
    }

    #[test]
    fn rotation_does_not_change_density() {
        // Density is intrinsic to the panel — rotating it doesn't change how
        // many pixels it packs per millimetre.
        let mut p = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let density_unrotated = p.effective_density_px_per_mm();
        p.rotation = Rotation::R90;
        assert_eq!(p.effective_density_px_per_mm(), density_unrotated);
    }

    #[test]
    fn rotated_placement_derives_rotated_stage_rect() {
        let mut p = placement(0, [10.0, 20.0], [500.0, 1000.0], [1080, 1920]);
        p.rotation = Rotation::R90;
        let layout = StageLayout {
            placements: vec![p],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands[0].stage_rect_mm, [10.0, 20.0, 1000.0, 500.0]);
    }

    // ── Packing ──

    #[test]
    fn single_island_atlas_has_no_extra_gutter() {
        // 540x960mm at 1080x1920 native = an exact 2.0 px/mm on both axes, so
        // the width-axis density assumption reproduces native_resolution
        // exactly (a real, square-pixel panel).
        let p = placement(0, [0.0, 0.0], [540.0, 960.0], [1080, 1920]);
        let layout = StageLayout {
            placements: vec![p],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands.len(), 1);
        assert_eq!(derived.islands[0].atlas_region_px, [0, 0, 1080, 1920]);
        assert_eq!(derived.atlas_size, [1080, 1920]);
    }

    #[test]
    fn two_islands_pack_with_one_gutter_between() {
        let a = placement(0, [0.0, 0.0], [540.0, 960.0], [1080, 1920]);
        let b = placement(1, [3500.0, 0.0], [540.0, 960.0], [1080, 1920]);
        let layout = StageLayout {
            placements: vec![a, b],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands[0].atlas_region_px, [0, 0, 1080, 1920]);
        assert_eq!(
            derived.islands[1].atlas_region_px,
            [1080 + ATLAS_GUTTER_PX, 0, 1080, 1920]
        );
        assert_eq!(derived.atlas_size, [1080 * 2 + ATLAS_GUTTER_PX, 1920]);
    }

    #[test]
    fn total_atlas_pixels_track_owned_hardware_not_stage_width() {
        // Moving islands further apart must never change rendered pixel count
        // (§8, §9 — "dragging displays apart never changes it").
        let a = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let b_near = placement(1, [3500.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let b_far = placement(1, [50_000.0, 0.0], [500.0, 1000.0], [1080, 1920]);

        let near = derive_stage(&StageLayout {
            placements: vec![a.clone(), b_near],
        });
        let far = derive_stage(&StageLayout {
            placements: vec![a, b_far],
        });
        assert_eq!(near.atlas_size, far.atlas_size);
    }

    // ── Density ──

    #[test]
    fn island_density_is_the_densest_members_native_density() {
        // Two abutting panels of different native density — the island must
        // use the denser one so nothing is under-sampled.
        let a = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]); // ~2.16 px/mm
        let b = placement(1, [500.0, 0.0], [500.0, 1000.0], [2160, 3840]); // ~4.32 px/mm
        let layout = StageLayout {
            placements: vec![a, b],
        };
        let derived = derive_stage(&layout);
        assert_eq!(derived.islands.len(), 1);
        let expected = (2160.0f32 / 500.0).max(1080.0 / 500.0);
        assert!((derived.islands[0].px_per_mm - expected).abs() < 0.001);
    }

    #[test]
    fn density_cap_lowers_effective_density_but_never_raises_it() {
        let mut p = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let native = p.effective_density_px_per_mm();

        p.advanced.density_cap_px_per_mm = Some(native * 2.0);
        assert_eq!(
            p.effective_density_px_per_mm(),
            native,
            "a cap above native must not upscale"
        );

        p.advanced.density_cap_px_per_mm = Some(native * 0.5);
        assert!((p.effective_density_px_per_mm() - native * 0.5).abs() < 0.001);
    }

    #[test]
    fn density_cap_lowers_packed_atlas_pixels() {
        let mut p = placement(0, [0.0, 0.0], [500.0, 1000.0], [1080, 1920]);
        let uncapped = derive_stage(&StageLayout {
            placements: vec![p.clone()],
        });
        let native = p.effective_density_px_per_mm();
        p.advanced.density_cap_px_per_mm = Some(native * 0.5);
        let capped = derive_stage(&StageLayout {
            placements: vec![p],
        });
        assert!(capped.atlas_size[0] < uncapped.atlas_size[0]);
        assert!(capped.atlas_size[1] < uncapped.atlas_size[1]);
    }

    // ── OutputId / StageLayout helpers ──

    #[test]
    fn next_output_id_is_one_past_the_max() {
        let layout = StageLayout {
            placements: vec![
                placement(0, [0.0, 0.0], [1.0, 1.0], [1, 1]),
                placement(5, [0.0, 0.0], [1.0, 1.0], [1, 1]),
            ],
        };
        assert_eq!(layout.next_output_id(), OutputId(6));
        assert_eq!(StageLayout::default().next_output_id(), OutputId(0));
    }

    #[test]
    fn find_and_find_mut_locate_by_id() {
        let mut layout = StageLayout {
            placements: vec![placement(3, [0.0, 0.0], [1.0, 1.0], [1, 1])],
        };
        assert!(layout.find(OutputId(3)).is_some());
        assert!(layout.find(OutputId(4)).is_none());
        layout.find_mut(OutputId(3)).unwrap().name = "Renamed".into();
        assert_eq!(layout.find(OutputId(3)).unwrap().name, "Renamed");
    }

    // ── Serde defaults ──

    #[test]
    fn missing_stage_layout_json_deserializes_to_empty_default() {
        #[derive(Debug, Serialize, Deserialize, Default)]
        #[serde(default)]
        struct Wrapper {
            stage_layout: StageLayout,
        }
        let w: Wrapper = serde_json::from_str("{}").unwrap();
        assert!(w.stage_layout.is_empty());
    }

    #[test]
    fn display_placement_round_trips_through_json() {
        let mut p = placement(7, [12.5, -3.0], [500.0, 1000.0], [1080, 1920]);
        p.rotation = Rotation::R90;
        p.identity = Some(DisplayIdentity {
            uuid: Some("ABC-123".into()),
            name: "Totem L".into(),
        });
        p.advanced.density_cap_px_per_mm = Some(3.0);

        let json = serde_json::to_string(&p).unwrap();
        let back: DisplayPlacement = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
