//! `SceneObject` — the CPU struct carried on [`PortType::Object`](crate::node_graph::ports::PortType::Object)
//! wires.
//!
//! SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D2: an object today is "whatever named
//! group happens to wrap the wires feeding `mesh_k`" — a UI-side convention
//! `SceneVm` reverse-engineers. This struct makes it a typed fact instead:
//! `node.scene_object` binds mesh + transform + material + maps + instances
//! into one value, produced once per frame and read by `render_scene`
//! through the same slot-resolution calls it already makes
//! (`array_slot`, `texture_2d_slot`, `slot_generation_of` —
//! `bindings.rs:190-196`, `render_scene.rs:2436-2450`) — the Object just
//! changes where the slot comes from, not how it resolves.
//!
//! `Copy`, zero allocation — hot-path legal by construction, same cost
//! class as [`Camera`](crate::node_graph::camera::Camera) /
//! [`Light`](crate::node_graph::light::Light). CPU facts (`visible`,
//! `transform`, `material`) are carried by value; GPU resources (mesh, maps,
//! instances) are carried as [`Slot`]s, resolved by the consumer exactly as
//! today.

use crate::node_graph::bindings::Slot;
use crate::node_graph::material::Material;
use crate::node_graph::transform::Transform;

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
    /// Local TRS. Identity ([`Transform::default`]) when the `transform`
    /// input port is unwired.
    pub transform: Transform,
    /// Shading description. `None` when the `material` input port is
    /// unwired — consumers treat this the same as an unwired `material_k`
    /// port does today (a structured error, per the Material design doc's
    /// "no silent fallbacks" rule).
    pub material: Option<Material>,
    /// `Array<MeshVertex>` slot. `None` when the `vertices` input port is
    /// unwired — consumers skip the draw the same way an unresolved
    /// `mesh_k` slot is skipped today (`render_scene.rs:2437`).
    pub mesh: Option<Slot>,
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
    /// `Texture2D` slot — sheen color map (GLTF_MATERIAL_EXTENSIONS_DESIGN
    /// E3/E4/E5).
    pub sheen_color_map: Option<Slot>,
    /// `Texture2D` slot — sheen roughness map.
    pub sheen_roughness_map: Option<Slot>,
    /// `Texture2D` slot — iridescence map.
    pub iridescence_map: Option<Slot>,
    /// `Texture2D` slot — iridescence thickness map.
    pub iridescence_thickness_map: Option<Slot>,
    /// `Texture2D` slot — anisotropy map.
    pub anisotropy_map: Option<Slot>,
    /// `Texture2D` slot — clearcoat map (GLTF_MATERIAL_EXTENSIONS_DESIGN E6).
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
    /// `Texture2D` slot — volume thickness map.
    pub volume_thickness_map: Option<Slot>,
    /// `Array<InstanceTransform>` slot, for instanced draws. `None` for a
    /// single-instance object.
    pub instances: Option<Slot>,
}

// Invariant (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md §4): `SceneObject` stays
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_shaped_object_is_invisible_with_no_resources() {
        // No `Default` impl is declared (every field's "sensible unwired
        // value" already lives on the producing primitive's own defaults —
        // `Transform::default()` for the identity transform, `None` for
        // every optional resource); this test just documents the fully-
        // empty construction reads as inert.
        let obj = SceneObject {
            visible: false,
            transform: Transform::default(),
            material: None,
            mesh: None,
            base_color_map: None,
            normal_map: None,
            mr_map: None,
            occlusion_map: None,
            emissive_map: None,
            sheen_color_map: None,
            sheen_roughness_map: None,
            iridescence_map: None,
            iridescence_thickness_map: None,
            anisotropy_map: None,
            clearcoat_map: None,
            clearcoat_roughness_map: None,
            clearcoat_normal_map: None,
            specular_map: None,
            specular_color_map: None,
            transmission_map: None,
            volume_thickness_map: None,
            instances: None,
        };
        assert!(!obj.visible);
        assert!(obj.mesh.is_none());
        assert!(obj.material.is_none());
    }

    #[test]
    fn scene_object_is_copy_and_cheap_to_clone() {
        let obj = SceneObject {
            visible: true,
            transform: Transform::default(),
            material: None,
            mesh: Some(Slot(0)),
            base_color_map: Some(Slot(1)),
            normal_map: None,
            mr_map: None,
            occlusion_map: None,
            emissive_map: None,
            sheen_color_map: None,
            sheen_roughness_map: None,
            iridescence_map: None,
            iridescence_thickness_map: None,
            anisotropy_map: None,
            clearcoat_map: None,
            clearcoat_roughness_map: None,
            clearcoat_normal_map: None,
            specular_map: None,
            specular_color_map: None,
            transmission_map: None,
            volume_thickness_map: None,
            instances: Some(Slot(2)),
        };
        let copy = obj;
        // Both usable — proves Copy, not just Clone (a move would make
        // `obj` unusable below).
        assert_eq!(obj, copy);
    }
}
