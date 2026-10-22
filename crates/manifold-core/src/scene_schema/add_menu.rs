//! The Scene tab's Add menu as data: one entry per thing the menu adds. A
//! lane that makes a new kind addable appends its entries here; the panel and
//! the app read this list and nothing else.

use crate::scene_doc::{
    CameraFields, CameraRig, ForceFields, ForceType, ItemFields, ItemKind, LightFields, LightType, ObjectFields, Shape, SourceRole,
    WaterSourceFields,
};

/// One thing the Add menu adds: an item of `kind` whose fields are
/// `fields()`, named `name`. An entry with no `fields` opens the model file
/// picker and imports the picked file as a drop does: a Group of its parts,
/// or one Object for a single-mesh file.
#[derive(Clone, Copy)]
pub struct AddEntry {
    /// What a click sends.
    pub key: &'static str,
    /// The menu section it sits under.
    pub group: &'static str,
    pub label: &'static str,
    pub kind: ItemKind,
    /// The new item's name; a name already taken gets a number.
    pub name: &'static str,
    pub fields: Option<fn() -> ItemFields>,
}

impl std::fmt::Debug for AddEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddEntry").field("key", &self.key).finish()
    }
}

fn object(shape: Shape) -> ItemFields {
    ItemFields::Object(ObjectFields { shape, ..ObjectFields::default() })
}

fn light(light: LightType) -> ItemFields {
    ItemFields::Light(LightFields { light })
}

fn force(force: ForceType) -> ItemFields {
    ItemFields::Force(ForceFields { force, ..ForceFields::default() })
}

fn source(role: SourceRole) -> ItemFields {
    ItemFields::WaterSource(WaterSourceFields { role, ..WaterSourceFields::default() })
}

const fn entry(
    key: &'static str,
    group: &'static str,
    label: &'static str,
    kind: ItemKind,
    name: &'static str,
    fields: fn() -> ItemFields,
) -> AddEntry {
    AddEntry { key, group, label, kind, name, fields: Some(fields) }
}

/// The entry that imports a model file the user picks.
const fn model_file(key: &'static str, group: &'static str, label: &'static str) -> AddEntry {
    AddEntry { key, group, label, kind: ItemKind::Object, name: "", fields: None }
}

/// Every Add menu entry, in menu order.
pub static ADD_ENTRIES: [AddEntry; 27] = [
    entry("object.cube", "Object", "Cube", ItemKind::Object, "Cube", || object(Shape::Cube)),
    entry("object.sphere", "Object", "Sphere", ItemKind::Object, "Sphere", || object(Shape::Sphere)),
    entry("object.plane", "Object", "Plane", ItemKind::Object, "Plane", || object(Shape::Plane)),
    entry("object.layer_plane", "Object", "Layer Plane", ItemKind::Object, "Layer Plane", || object(Shape::Plane)),
    entry("object.tetrahedron", "Object", "Tetrahedron", ItemKind::Object, "Tetrahedron", || object(Shape::Tetrahedron)),
    entry("object.octahedron", "Object", "Octahedron", ItemKind::Object, "Octahedron", || object(Shape::Octahedron)),
    entry("object.icosahedron", "Object", "Icosahedron", ItemKind::Object, "Icosahedron", || object(Shape::Icosahedron)),
    entry("object.dodecahedron", "Object", "Dodecahedron", ItemKind::Object, "Dodecahedron", || object(Shape::Dodecahedron)),
    entry("light.sun", "Light", "Sun", ItemKind::Light, "Sun", || light(LightType::Sun)),
    entry("light.point", "Light", "Point", ItemKind::Light, "Point Light", || light(LightType::Point)),
    entry("light.spot", "Light", "Spot", ItemKind::Light, "Spot Light", || light(LightType::Spot)),
    entry("camera", "Camera", "Camera", ItemKind::Camera, "Camera", || {
        ItemFields::Camera(CameraFields { rig: CameraRig::Orbit, ..CameraFields::default() })
    }),
    entry("group", "Group", "Group", ItemKind::Group, "Group", || ItemFields::default_for(ItemKind::Group)),
    entry("material", "Material", "Material", ItemKind::Material, "Material", || ItemFields::default_for(ItemKind::Material)),
    entry("swarm", "Swarm", "Swarm", ItemKind::Swarm, "Swarm", || ItemFields::default_for(ItemKind::Swarm)),
    entry("water", "Water", "Water", ItemKind::Water, "Water", || ItemFields::default_for(ItemKind::Water)),
    entry("water_source.fill", "Water Source", "Fill", ItemKind::WaterSource, "Fill", || source(SourceRole::Fill)),
    entry("water_source.inflow", "Water Source", "Inflow", ItemKind::WaterSource, "Inflow", || source(SourceRole::Inflow)),
    entry("water_source.drain", "Water Source", "Drain", ItemKind::WaterSource, "Drain", || source(SourceRole::Drain)),
    entry("force.radial", "Force", "Radial", ItemKind::Force, "Radial Force", || force(ForceType::Radial)),
    entry("force.uniform", "Force", "Uniform", ItemKind::Force, "Uniform Force", || force(ForceType::Uniform)),
    entry("force.vortex", "Force", "Vortex", ItemKind::Force, "Vortex", || force(ForceType::Vortex)),
    entry("force.wind", "Force", "Wind", ItemKind::Force, "Wind", || force(ForceType::Wind)),
    entry("force.drag", "Force", "Drag", ItemKind::Force, "Drag", || force(ForceType::Drag)),
    entry("force.attractor", "Force", "Attractor", ItemKind::Force, "Attractor", || force(ForceType::Attractor)),
    entry("force.explosion", "Force", "Explosion", ItemKind::Force, "Explosion", || force(ForceType::Explosion)),
    model_file("object.mesh", "Object", "Mesh from file\u{2026}"),
];

/// The entry a click on `key` means.
pub fn add_entry(key: &str) -> Option<&'static AddEntry> {
    ADD_ENTRIES.iter().find(|entry| entry.key == key)
}

/// A name for a new item: `base`, or `base N` for the lowest N from 2 that no
/// item in `taken` has.
pub fn unique_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_owned();
    }
    (2..).map(|n| format!("{base} {n}")).find(|name| !taken.contains(&name.as_str())).expect("an unused number")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_makes_fields_of_its_kind_and_keys_are_unique() {
        for entry in &ADD_ENTRIES {
            if let Some(fields) = entry.fields {
                assert_eq!(fields().kind(), entry.kind, "{}", entry.key);
            }
            assert_eq!(ADD_ENTRIES.iter().filter(|other| other.key == entry.key).count(), 1, "{}", entry.key);
        }
    }

    #[test]
    fn a_taken_name_gets_the_lowest_free_number() {
        assert_eq!(unique_name("Cube", &["Cube", "Cube 2", "Cube 4"]), "Cube 3");
        assert_eq!(unique_name("Cube", &[]), "Cube");
    }
}
