//! `SceneObject` — the CPU struct carried on [`PortType::Object`](crate::node_graph::ports::PortType::Object)
//! wires.
//!
//! An object today is "whatever named
//! group happens to wrap the wires feeding `mesh_k`" — a UI-side convention
//! `SceneVm` reverse-engineers. This struct makes it a typed fact instead:
//! `node.scene_object` binds mesh + transform + material + maps + instances
//! into one value, produced once per frame and read by `render_scene`
//! through the same slot-resolution calls it already makes
//! (`array_slot`, `texture_2d_slot`, `storage_revision_of` —
//! `bindings.rs:190-196`, `render_scene.rs:2436-2450`) — the Object just
//! changes where the slot comes from, not how it resolves.
//!
//! `Copy`, zero allocation — hot-path legal by construction, same cost
//! class as [`Camera`](crate::node_graph::camera::Camera) /
//! [`Light`](crate::node_graph::light::Light). CPU facts (`visible`,
//! `transform`, `material`) are carried by value; GPU resources (mesh, maps,
//! instances) are carried as [`Slot`]s, resolved by the consumer exactly as
//! today.

use crate::bindings::Slot;
use crate::scene::material::Material;
use crate::scene::transform::Transform;

/// One scene object's full bundle — mesh + transform + material + maps +
/// instances — carried on a single [`PortType::Object`](crate::node_graph::ports::PortType::Object)
/// wire. See the module doc for the design rationale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SceneObject {
    /// Whether this object draws (and casts shadows) this frame.
    /// Port-shadowed on `node.scene_object`'s `visible` input — "mute the
    /// statue on the drop" is a MIDI binding, not a feature request.
    /// `false` = no draw AND no shadow cast (an invisible object leaves
    /// no shadow).
    pub visible: bool,
    /// Whether this object casts shadows onto direct-light shadowing
    /// (raster shadow maps + the RT shadow-ray pass). `false` removes it
    /// ONLY from that: it still occludes AO, feeds GI, and appears in
    /// reflections and primary hits, on both the raster and RT paths —
    /// mirrors `node.light`'s `cast_shadows` param
    /// (`primitives/light.rs`), same "float 0..1, modulatable" shape.
    pub cast_shadows: bool,
    /// The mesh is closed and outward-wound, so an opaque draw may skip its
    /// back faces. Set by the scene compiler for built-in solids.
    pub closed: bool,
    /// Local TRS. Identity ([`Transform::default`]) when the `transform`
    /// input port is unwired.
    pub transform: Transform,
    /// Optional parent TRS. The renderer composes this before `transform`,
    /// preserving the full parent matrix (including shear introduced by
    /// non-uniform scale and a rotated child).
    pub parent_transform: Option<Transform>,
    /// Shading description. `None` when the `material` input port is
    /// unwired — consumers treat this the same as an unwired `material_k`
    /// port does today (a structured error, per the Material design doc's
    /// "no silent fallbacks" rule).
    pub material: Option<Material>,
    /// `Array<MeshVertex>` slot. `None` when the `vertices` input port is
    /// unwired — consumers skip the draw the same way an unresolved
    /// `mesh_k` slot is skipped today (`render_scene.rs:2437`).
    pub mesh: Option<Slot>,
    /// Optional u32 triangle indices into the vertex array.
    pub indices: Option<Slot>,
    /// Optional per-vertex appearance weights consumed by `render_scene`.
    pub weights: Option<Slot>,
    /// `Array<Vec4Vertex>` slot: each vertex's position a frame ago, from a
    /// producer whose triangles change every frame (a liquid's surface mesh),
    /// so the renderer's own vertex history cannot give its motion. `None`
    /// for every other mesh.
    pub prev_positions: Option<Slot>,
    /// Cut-map slot. The renderer resolves its typed content metadata from
    /// the slot, just as it does for mesh, weights, and map resources.
    pub topology: Option<Slot>,
    /// Water drawn straight from a level set instead of a mesh. `None` for
    /// mesh objects; an object with a field draws the field.
    pub field: Option<FieldSurface>,
    /// Smoke or fire in a box of cells (`node.gas_volume`), drawn by the
    /// renderer's volume pass; an object with a gas draws no mesh.
    pub gas: Option<GasVolume>,
    /// The Materials covering the object's own where its material's cover
    /// rules say, Cover 1 then Cover 2.
    pub covers: [Option<Cover>; 2],
    /// `Texture2D` slot — base colour map.
    pub base_color_map: Option<Slot>,
    /// `Texture2D` slot — normal map.
    pub normal_map: Option<Slot>,
    /// `Texture2D` slot — metallic/roughness map.
    pub mr_map: Option<Slot>,
    /// `Texture2D` slot — ambient occlusion map.
    pub occlusion_map: Option<Slot>,
    /// `Texture2D` slot — emissive map.
    pub emissive_map: Option<Slot>,
    /// `Texture2D` slot — sheen color map.
    pub sheen_color_map: Option<Slot>,
    /// `Texture2D` slot — sheen roughness map.
    pub sheen_roughness_map: Option<Slot>,
    /// `Texture2D` slot — iridescence map.
    pub iridescence_map: Option<Slot>,
    /// `Texture2D` slot — iridescence thickness map.
    pub iridescence_thickness_map: Option<Slot>,
    /// `Texture2D` slot — anisotropy map.
    pub anisotropy_map: Option<Slot>,
    /// `Texture2D` slot — clearcoat map.
    pub clearcoat_map: Option<Slot>,
    /// `Texture2D` slot — clearcoat roughness map.
    pub clearcoat_roughness_map: Option<Slot>,
    /// `Texture2D` slot — clearcoat normal map.
    pub clearcoat_normal_map: Option<Slot>,
    /// `Texture2D` slot — specular map.
    pub specular_map: Option<Slot>,
    /// `Texture2D` slot — specular color map.
    pub specular_color_map: Option<Slot>,
    /// `Texture2D` slot — transmission map.
    pub transmission_map: Option<Slot>,
    /// `Texture2D` slot — diffuse-transmission factor map (A channel).
    pub diffuse_transmission_map: Option<Slot>,
    /// `Texture2D` slot — diffuse-transmission colour map (RGB channels).
    pub diffuse_transmission_color_map: Option<Slot>,
    /// `Texture2D` slot — volume thickness map.
    pub volume_thickness_map: Option<Slot>,
    /// `Array<InstanceTransform>` slot, for instanced draws. `None` for a
    /// single-instance object.
    pub instances: Option<Slot>,
    /// Optional live instance count for an instanced draw. `None` preserves
    /// the existing capacity-based behavior; wired values are clamped by the
    /// renderer to the backing buffer's capacity.
    pub instance_count: Option<f32>,
    /// Where `instances` place this object. See [`InstanceSpace`].
    pub instances_space: InstanceSpace,
    /// Per-object emissive-map strength multiplier. Lives on the object, not
    /// the material, so a skin driving `emissive_map` can be dimmed without
    /// touching the underlying material emission factor. Default 1.0.
    pub emission_strength: f32,
    /// Per-object appearance gain. Default 1.0.
    pub gain: f32,
}

/// The level set's canonical value, in cells: how far from any water a node in
/// a tile the world-tile table does not hold is taken to be. A display level
/// set carries `FIELD_FAR_CELLS * cell_size` as its last, canonical element.
pub const FIELD_FAR_CELLS: f32 = 3.0;

/// A level-set surface the renderer marches to its zero crossing instead
/// of rasterising triangles. Every array is a pooled field of
/// `world_tiles::pool_len(pool_tiles)` elements laid out under the world-tile
/// table in `tiles`: node p of the display lattice sits at `(p - 1/2) *
/// cell_size` in the object's space, and a node in a tile the table does not
/// hold reads the array's last, canonical element.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FieldSurface {
    /// `Array<f32>`: signed distance in metres at each node, negative inside.
    pub levelset: Slot,
    /// Optional `Array<f32>`: three velocity components (m/s) per node, for
    /// motion vectors and the ripple detail's flow. `None` draws the field
    /// as still.
    pub velocity: Option<Slot>,
    /// Optional `Array<FluidParticle>`: the displayed foam particles, each
    /// one's w its fade (0 an empty slot), which the renderer draws onto the
    /// surface at pixel resolution. Not a pooled field. `None` draws no foam.
    pub foam: Option<Slot>,
    /// Times the foam particles' area: 1 is the foam they make, above 0.
    pub foam_coverage: f32,
    /// Optional `Array<f32>`: mist extinction per node, per metre: the spray
    /// droplets in the air, which the march integrates under Beer-Lambert.
    /// `None` draws no mist.
    pub mist: Option<Slot>,
    /// Optional `Array<u32>`: the water's wet map (`wet_map`), the soft wet
    /// marks the water left on surfaces, which the scene draws over what it
    /// sees before every mesh shades. Not a pooled field. `None` wets nothing.
    pub wet_map: Option<Slot>,
    /// `Array<u32>`: the world-tile table the arrays above are laid out under.
    pub tiles: Slot,
    /// `Array<f32>`: eight floats, the held tiles' node bounds in object
    /// space (min xyz, 0, max xyz, 0); min above max when no tile is held.
    pub extent: Slot,
    /// Tile slots in the pool, at least 1.
    pub pool_tiles: u32,
    /// Lattice cell size in metres, above zero.
    pub cell_size: f32,
    /// Ripple detail strength over the automatic one: 1 is the automatic strength, 0 turns it off.
    pub ripple_detail: f32,
    /// How strongly the sunlight the surface focuses shows on what lies under
    /// it: 1 is physical, 0 turns caustics off.
    pub caustics: f32,
}

/// A Material covering an object's own: its values (Tile Size, Break
/// Repeats, colour, roughness) and its base colour, normal (height in alpha)
/// and metallic-roughness map slots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cover {
    pub material: Material,
    pub maps: [Option<Slot>; 3],
}

/// A Smoke's or Fire's volume this frame: the solver's fields on a dense,
/// axis-aligned box of cells, the look the renderer marches it with, and the
/// fire's light.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GasVolume {
    /// `Array<u32>`, two words a cell in order x + nx·(y + ny·z): half
    /// floats (density, temperature) then (fuel, flame).
    pub state: Slot,
    /// `Array<u32>`, two words a cell: half floats (u, v) then (w, 0), m/s.
    pub velocity: Slot,
    /// World position of the box's min corner; cell (i, j, k) spans
    /// `origin + [i, i + 1] · cell_size` on each axis.
    pub origin: [f32; 3],
    pub cell_size: f32,
    pub dims: [u32; 3],
    pub look: GasLook,
    pub light: GasLight,
}

/// How a gas draws.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GasLook {
    pub smoke_color: [f32; 3],
    /// Extinction per metre at density 1.
    pub density: f32,
    /// Single-scattering albedo, 0 to 1.
    pub albedo: f32,
    /// Detail noise strength, 0 to 1.
    pub detail: f32,
    /// Flame radiance at full flame and the 1500 K reference, in the
    /// scene's light units.
    pub flame_brightness: f32,
    /// Flame colour temperature in kelvin at normalised temperature 0 and 1.
    pub kelvin_cool: f32,
    pub kelvin_hot: f32,
    /// Fire glow on the smoke, times the flame brightness.
    pub glow: f32,
    /// The fire's point light, times its emission sum; 0 adds no light.
    pub light_brightness: f32,
    /// The render preset: 0 Low, 1 Medium, 2 High, 3 Extra High.
    pub quality: u32,
}

/// The fire's light as the solver read it back: the flames' emission-weighted
/// centre in the world, their emission sum (0 for none) and their
/// emission-weighted normalised temperature.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GasLight {
    pub position: [f32; 3],
    pub power: f32,
    pub temperature: f32,
}

/// The space an object's instances live in. Nested group transforms are
/// composed on the CPU, so the renderer sees one parent matrix per draw.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstanceSpace {
    /// world = parent · local · instance. Echoes, scatter and
    /// swarms: the copies live in the object's own space.
    #[default]
    Object,
    /// world = parent · instance · local. Copies of a group: each instance
    /// places the whole group and the object's transform puts the part
    /// inside every copy.
    Group,
    /// world = instance · parent · local. Scene-wide corridor tiling and
    /// planar reflection (`Scene Loop`, `Scene Mirror`): each instance places
    /// the whole assembled world-space object along the world axis.
    World,
}

impl InstanceSpace {
    /// The `instances_space` enum param index (Object 0, Group 1, World 2).
    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Group,
            2 => Self::World,
            _ => Self::Object,
        }
    }
}

// Invariant: `SceneObject` stays
// `Copy` — hot-path legal by construction, no per-frame allocation. A type
// that stops being `Copy` fails this assertion at compile time instead of
// silently degrading the hot path.
const _: () = {
    fn assert_copy<T: Copy>() {}
    fn check(s: SceneObject) {
        assert_copy::<SceneObject>();
        let _ = s;
    }
    let _ = check;
};

/// Tracks connectivity independently from per-frame vertex positions.
#[derive(Default)]
pub struct MeshTopologyHistory(Option<u64>);

impl MeshTopologyHistory {
    pub fn update(
        &mut self,
        resource_epoch: u64,
        revisions: impl Iterator<Item = impl std::hash::Hash>,
    ) -> bool {
        use std::hash::{Hash, Hasher};
        let mut hash = ahash::AHasher::default();
        resource_epoch.hash(&mut hash);
        for (object, revision) in revisions.enumerate() {
            object.hash(&mut hash);
            revision.hash(&mut hash);
        }
        let current = hash.finish();
        self.0
            .replace(current)
            .is_some_and(|previous| previous != current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::content_revision::ContentVersion;

    #[test]
    fn cut_topology_resets_history_without_resetting_ordinary_motion() {
        let mut history = MeshTopologyHistory::default();
        let revision = Some(ContentVersion::new(
            1,
            crate::exec::execution_plan::ResourceId(4),
            1,
        ));
        assert!(!history.update(1, [None, revision].into_iter()));
        // Deformation changes the mesh buffer, not this cut-map revision.
        assert!(!history.update(1, [None, revision].into_iter()));
        assert!(
            history.update(
                1,
                [
                    None,
                    Some(ContentVersion::new(
                        1,
                        crate::exec::execution_plan::ResourceId(4),
                        2
                    ))
                ]
                .into_iter(),
            )
        );
        assert!(
            !history.update(
                1,
                [
                    None,
                    Some(ContentVersion::new(
                        1,
                        crate::exec::execution_plan::ResourceId(4),
                        2
                    ))
                ]
                .into_iter(),
            )
        );
        // Recycled resources, changed object order and removal also reset.
        assert!(history.update(2, [None, revision].into_iter()));
        assert!(history.update(2, [revision, None].into_iter()));
        assert!(history.update(2, [None::<ContentVersion>, None].into_iter()));
        assert!(!history.update(2, [None::<ContentVersion>, None].into_iter()));
    }

    #[test]
    fn general_topology_revision_resets_history_without_a_cut_map() {
        let mut history = MeshTopologyHistory::default();
        let object = (Some(Slot(4)), Some(1_u64), None::<ContentVersion>);
        assert!(!history.update(1, [object].into_iter()));
        assert!(!history.update(1, [object].into_iter()));
        assert!(history.update(1, [(object.0, Some(2_u64), object.2)].into_iter()));
    }
}
