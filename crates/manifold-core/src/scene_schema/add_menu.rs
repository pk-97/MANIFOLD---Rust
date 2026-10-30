//! The Scene tab's Add menu as data: one entry per thing the menu adds. The
//! panel and the app read this list and nothing else.

use crate::scene_doc::{
    CameraFields, CameraRig, ForceFields, ForceType, ItemFields, ItemKind, LightFields, LightType,
    Matter, ModifierFields, ModifierType, ObjectFields, SandFields, Shape, SourceRole, WaterDraw,
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
    ItemFields::Object(ObjectFields {
        shape,
        ..ObjectFields::default()
    })
}

fn light(light: LightType) -> ItemFields {
    ItemFields::Light(LightFields { light })
}

fn force(force: ForceType) -> ItemFields {
    ItemFields::Force(ForceFields {
        force,
        ..ForceFields::default()
    })
}

fn source(role: SourceRole) -> ItemFields {
    ItemFields::WaterSource(WaterSourceFields {
        role,
        ..WaterSourceFields::default()
    })
}

fn modifier(modifier: ModifierType) -> ItemFields {
    ItemFields::Modifier(ModifierFields {
        modifier,
        ..ModifierFields::default()
    })
}

const fn entry(
    key: &'static str,
    group: &'static str,
    label: &'static str,
    kind: ItemKind,
    name: &'static str,
    fields: fn() -> ItemFields,
) -> AddEntry {
    AddEntry {
        key,
        group,
        label,
        kind,
        name,
        fields: Some(fields),
    }
}

/// The entry that imports a model file the user picks.
const fn model_file(key: &'static str, group: &'static str, label: &'static str) -> AddEntry {
    AddEntry {
        key,
        group,
        label,
        kind: ItemKind::Object,
        name: "",
        fields: None,
    }
}

/// Every Add menu entry, in menu order.
pub static ADD_ENTRIES: [AddEntry; 68] = [
    entry(
        "object.cube",
        "Object",
        "Cube",
        ItemKind::Object,
        "Cube",
        || object(Shape::Cube),
    ),
    entry(
        "object.sphere",
        "Object",
        "Sphere",
        ItemKind::Object,
        "Sphere",
        || object(Shape::Sphere),
    ),
    entry(
        "object.plane",
        "Object",
        "Plane",
        ItemKind::Object,
        "Plane",
        || object(Shape::Plane),
    ),
    entry(
        "object.layer_plane",
        "Object",
        "Layer Plane",
        ItemKind::Object,
        "Layer Plane",
        || object(Shape::Plane),
    ),
    entry(
        "object.tetrahedron",
        "Object",
        "Tetrahedron",
        ItemKind::Object,
        "Tetrahedron",
        || object(Shape::Tetrahedron),
    ),
    entry(
        "object.octahedron",
        "Object",
        "Octahedron",
        ItemKind::Object,
        "Octahedron",
        || object(Shape::Octahedron),
    ),
    entry(
        "object.icosahedron",
        "Object",
        "Icosahedron",
        ItemKind::Object,
        "Icosahedron",
        || object(Shape::Icosahedron),
    ),
    entry(
        "object.dodecahedron",
        "Object",
        "Dodecahedron",
        ItemKind::Object,
        "Dodecahedron",
        || object(Shape::Dodecahedron),
    ),
    entry("light.sun", "Light", "Sun", ItemKind::Light, "Sun", || {
        light(LightType::Sun)
    }),
    entry(
        "light.point",
        "Light",
        "Point",
        ItemKind::Light,
        "Point Light",
        || light(LightType::Point),
    ),
    entry(
        "light.spot",
        "Light",
        "Spot",
        ItemKind::Light,
        "Spot Light",
        || light(LightType::Spot),
    ),
    entry(
        "camera",
        "Camera",
        "Camera",
        ItemKind::Camera,
        "Camera",
        || {
            ItemFields::Camera(CameraFields {
                rig: CameraRig::Orbit,
                ..CameraFields::default()
            })
        },
    ),
    entry("group", "Group", "Group", ItemKind::Group, "Group", || {
        ItemFields::default_for(ItemKind::Group)
    }),
    entry(
        "material",
        "Material",
        "Material",
        ItemKind::Material,
        "Material",
        || ItemFields::default_for(ItemKind::Material),
    ),
    entry("swarm", "Swarm", "Swarm", ItemKind::Swarm, "Swarm", || {
        ItemFields::default_for(ItemKind::Swarm)
    }),
    entry("water", "Water", "Water", ItemKind::Water, "Water", || {
        ItemFields::default_for(ItemKind::Water)
    }),
    entry("sand", "Matter", "Sand", ItemKind::Sand, "Sand", || {
        ItemFields::default_for(ItemKind::Sand)
    }),
    // Sand draws as grains; the others draw their surface, snow's grains
    // being far below a cell and shaded into it.
    entry(
        "matter.snow",
        "Matter",
        "Snow",
        ItemKind::Sand,
        "Snow",
        || matter(Matter::Snow, WaterDraw::Surface),
    ),
    entry(
        "matter.clay",
        "Matter",
        "Clay",
        ItemKind::Sand,
        "Clay",
        || matter(Matter::Clay, WaterDraw::Surface),
    ),
    entry("matter.mud", "Matter", "Mud", ItemKind::Sand, "Mud", || {
        matter(Matter::Mud, WaterDraw::Surface)
    }),
    entry("matter.goo", "Matter", "Goo", ItemKind::Sand, "Goo", || {
        matter(Matter::Goo, WaterDraw::Surface)
    }),
    entry(
        "matter.jelly",
        "Matter",
        "Jelly",
        ItemKind::Sand,
        "Jelly",
        || matter(Matter::Jelly, WaterDraw::Surface),
    ),
    entry("smoke", "Smoke", "Smoke", ItemKind::Smoke, "Smoke", || {
        ItemFields::default_for(ItemKind::Smoke)
    }),
    entry("fire", "Fire", "Fire", ItemKind::Fire, "Fire", || {
        ItemFields::default_for(ItemKind::Fire)
    }),
    entry(
        "water_source.fill",
        "Water Source",
        "Fill",
        ItemKind::WaterSource,
        "Fill",
        || source(SourceRole::Fill),
    ),
    entry(
        "water_source.inflow",
        "Water Source",
        "Inflow",
        ItemKind::WaterSource,
        "Inflow",
        || source(SourceRole::Inflow),
    ),
    entry(
        "water_source.drain",
        "Water Source",
        "Drain",
        ItemKind::WaterSource,
        "Drain",
        || source(SourceRole::Drain),
    ),
    // Push, Pull and Burst are one Radial card at three starting points (`FORCE_STARTS`).
    entry(
        "force.push",
        "Force",
        "Push",
        ItemKind::Force,
        "Push",
        || force(ForceType::Radial),
    ),
    entry(
        "force.pull",
        "Force",
        "Pull",
        ItemKind::Force,
        "Pull",
        || force(ForceType::Radial),
    ),
    entry(
        "force.burst",
        "Force",
        "Burst",
        ItemKind::Force,
        "Burst",
        || force(ForceType::Radial),
    ),
    entry(
        "force.uniform",
        "Force",
        "Uniform",
        ItemKind::Force,
        "Uniform Force",
        || force(ForceType::Uniform),
    ),
    entry(
        "force.vortex",
        "Force",
        "Vortex",
        ItemKind::Force,
        "Vortex",
        || force(ForceType::Vortex),
    ),
    entry(
        "force.wind",
        "Force",
        "Wind",
        ItemKind::Force,
        "Wind",
        || force(ForceType::Wind),
    ),
    entry(
        "force.drag",
        "Force",
        "Drag",
        ItemKind::Force,
        "Drag",
        || force(ForceType::Drag),
    ),
    // A new card aims at the selection, or the whole scene when nothing is
    // selected, at the end of the chain (section 7.2).
    entry(
        "modifier.surface_peel",
        "Modifier",
        "Surface Peel",
        ItemKind::Modifier,
        "Surface Peel",
        || modifier(ModifierType::SurfacePeel),
    ),
    entry(
        "modifier.ordered_recon",
        "Modifier",
        "Ordered Recon",
        ItemKind::Modifier,
        "Ordered Recon",
        || modifier(ModifierType::OrderedRecon),
    ),
    entry(
        "modifier.surface_waves",
        "Modifier",
        "Surface Waves",
        ItemKind::Modifier,
        "Surface Waves",
        || modifier(ModifierType::SurfaceWaves),
    ),
    entry(
        "modifier.spatial_echoes",
        "Modifier",
        "Spatial Echoes",
        ItemKind::Modifier,
        "Spatial Echoes",
        || modifier(ModifierType::SpatialEchoes),
    ),
    entry(
        "modifier.elastic_sculpture",
        "Modifier",
        "Elastic Sculpture",
        ItemKind::Modifier,
        "Elastic Sculpture",
        || modifier(ModifierType::ElasticSculpture),
    ),
    entry(
        "modifier.vortex_fragments",
        "Modifier",
        "Vortex Fragments",
        ItemKind::Modifier,
        "Vortex Fragments",
        || modifier(ModifierType::VortexFragments),
    ),
    entry(
        "modifier.shatter",
        "Modifier",
        "Shatter",
        ItemKind::Modifier,
        "Shatter",
        || modifier(ModifierType::Shatter),
    ),
    entry(
        "modifier.bend",
        "Modifier",
        "Bend",
        ItemKind::Modifier,
        "Bend",
        || modifier(ModifierType::Bend),
    ),
    entry(
        "modifier.twist",
        "Modifier",
        "Twist",
        ItemKind::Modifier,
        "Twist",
        || modifier(ModifierType::Twist),
    ),
    entry(
        "modifier.taper",
        "Modifier",
        "Taper",
        ItemKind::Modifier,
        "Taper",
        || modifier(ModifierType::Taper),
    ),
    entry(
        "modifier.inflate",
        "Modifier",
        "Inflate",
        ItemKind::Modifier,
        "Inflate",
        || modifier(ModifierType::Inflate),
    ),
    entry(
        "modifier.rotate",
        "Modifier",
        "Rotate",
        ItemKind::Modifier,
        "Rotate",
        || modifier(ModifierType::Rotate),
    ),
    entry(
        "modifier.displace_by_texture",
        "Modifier",
        "Displace by Texture",
        ItemKind::Modifier,
        "Displace by Texture",
        || modifier(ModifierType::DisplaceByTexture),
    ),
    entry(
        "modifier.morph",
        "Modifier",
        "Morph",
        ItemKind::Modifier,
        "Morph",
        || modifier(ModifierType::Morph),
    ),
    entry(
        "modifier.boil",
        "Modifier",
        "Boil",
        ItemKind::Modifier,
        "Boil",
        || modifier(ModifierType::Boil),
    ),
    entry(
        "modifier.ripple",
        "Modifier",
        "Ripple",
        ItemKind::Modifier,
        "Ripple",
        || modifier(ModifierType::Ripple),
    ),
    entry(
        "modifier.melt",
        "Modifier",
        "Melt",
        ItemKind::Modifier,
        "Melt",
        || modifier(ModifierType::Melt),
    ),
    entry(
        "modifier.glitch_jitter",
        "Modifier",
        "Glitch Jitter",
        ItemKind::Modifier,
        "Glitch Jitter",
        || modifier(ModifierType::GlitchJitter),
    ),
    entry(
        "modifier.shatter_mesh",
        "Modifier",
        "Shatter Mesh",
        ItemKind::Modifier,
        "Shatter Mesh",
        || modifier(ModifierType::ShatterMesh),
    ),
    entry(
        "modifier.slice",
        "Modifier",
        "Slice",
        ItemKind::Modifier,
        "Slice",
        || modifier(ModifierType::Slice),
    ),
    entry(
        "modifier.voxelize",
        "Modifier",
        "Voxelize",
        ItemKind::Modifier,
        "Voxelize",
        || modifier(ModifierType::Voxelize),
    ),
    entry(
        "modifier.fold",
        "Modifier",
        "Fold",
        ItemKind::Modifier,
        "Fold",
        || modifier(ModifierType::Fold),
    ),
    entry(
        "modifier.facet_normals",
        "Modifier",
        "Facet Normals",
        ItemKind::Modifier,
        "Facet Normals",
        || modifier(ModifierType::FacetNormals),
    ),
    entry(
        "modifier.wave_shear",
        "Modifier",
        "Wave Shear",
        ItemKind::Modifier,
        "Wave Shear",
        || modifier(ModifierType::WaveShear),
    ),
    entry(
        "modifier.cut_out_box",
        "Modifier",
        "Cut Out Box",
        ItemKind::Modifier,
        "Cut Out Box",
        || modifier(ModifierType::CutOutBox),
    ),
    entry(
        "modifier.shake",
        "Modifier",
        "Shake",
        ItemKind::Modifier,
        "Shake",
        || modifier(ModifierType::Shake),
    ),
    entry(
        "modifier.copy_wave",
        "Modifier",
        "Copy Wave",
        ItemKind::Modifier,
        "Copy Wave",
        || modifier(ModifierType::CopyWave),
    ),
    entry(
        "modifier.copy_jitter",
        "Modifier",
        "Copy Jitter",
        ItemKind::Modifier,
        "Copy Jitter",
        || modifier(ModifierType::CopyJitter),
    ),
    entry(
        "modifier.copy_noise",
        "Modifier",
        "Copy Noise",
        ItemKind::Modifier,
        "Copy Noise",
        || modifier(ModifierType::CopyNoise),
    ),
    entry(
        "modifier.copy_blend",
        "Modifier",
        "Copy Blend",
        ItemKind::Modifier,
        "Copy Blend",
        || modifier(ModifierType::CopyBlend),
    ),
    // Scene-wide: they act on the whole scene whatever is selected.
    entry(
        "modifier.scene_loop",
        "Modifier",
        "Scene Loop",
        ItemKind::Modifier,
        "Scene Loop",
        || modifier(ModifierType::SceneLoop),
    ),
    entry(
        "modifier.math_view",
        "Modifier",
        "Math View",
        ItemKind::Modifier,
        "Math View",
        || modifier(ModifierType::MathView),
    ),
    entry(
        "modifier.scene_mirror",
        "Modifier",
        "Scene Mirror",
        ItemKind::Modifier,
        "Scene Mirror",
        || modifier(ModifierType::SceneMirror),
    ),
    model_file("object.mesh", "Object", "Mesh from file\u{2026}"),
];

/// The entry a click on `key` means.
pub fn add_entry(key: &str) -> Option<&'static AddEntry> {
    ADD_ENTRIES.iter().find(|entry| entry.key == key)
}

/// The Radial card's three starting points: the card values each entry
/// writes after the recipe's own defaults. Push is the recipe as it stands.
/// Pull draws in with an inverse-square reach; Burst holds no steady push
/// and kicks outward when its Impulse fires.
pub const FORCE_STARTS: [(&str, &[(&str, f32)]); 3] = [
    ("force.push", &[]),
    (
        "force.pull",
        &[
            ("strength", -5.0),
            ("impulse_strength", -2.0),
            ("radius", 1.0),
            ("falloff", 2.0),
            ("curve", 1.0),
        ],
    ),
    (
        "force.burst",
        &[
            ("strength", 0.0),
            ("impulse_strength", 8.0),
            ("radius", 3.0),
        ],
    ),
];

/// Each Matter's surface: built at half the cell for edges a few
/// centimetres sharp, and smoothed as the material is. Snow keeps its
/// crumbs and mud its lumps; goo settles glassy smooth. Jelly and clay carry
/// their fill's own mesh and build no surface.
pub const MATTER_STARTS: [(&str, &[(&str, f32)]); 3] = [
    (
        "matter.snow",
        &[("surface_detail", 1.0), ("surface_smoothness", 0.5)],
    ),
    (
        "matter.mud",
        &[("surface_detail", 1.0), ("surface_smoothness", 1.0)],
    ),
    (
        "matter.goo",
        &[("surface_detail", 1.0), ("surface_smoothness", 2.0)],
    ),
];

/// The surface a Sand of `matter` starts with: its Add menu entry's starts.
pub fn matter_starts(matter: Matter) -> &'static [(&'static str, f32)] {
    entry_starts(match matter {
        Matter::Sand => "sand",
        Matter::Snow => "matter.snow",
        Matter::Clay => "matter.clay",
        Matter::Mud => "matter.mud",
        Matter::Goo => "matter.goo",
        Matter::Jelly => "matter.jelly",
    })
}

/// The card values the entry `key` starts at, beyond its recipe's defaults.
pub fn entry_starts(key: &str) -> &'static [(&'static str, f32)] {
    FORCE_STARTS
        .iter()
        .chain(&MATTER_STARTS)
        .find(|(entry, _)| *entry == key)
        .map_or(&[], |(_, starts)| starts)
}

/// A new Sand item made of `matter`, drawn `draw`.
fn matter(matter: Matter, draw: WaterDraw) -> ItemFields {
    ItemFields::Sand(SandFields {
        matter,
        draw,
        ..SandFields::default()
    })
}

/// A name for a new item: `base`, or `base N` for the lowest N from 2 that no
/// item in `taken` has.
pub fn unique_name(base: &str, taken: &[&str]) -> String {
    if !taken.contains(&base) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|name| !taken.contains(&name.as_str()))
        .expect("an unused number")
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
            assert_eq!(
                ADD_ENTRIES
                    .iter()
                    .filter(|other| other.key == entry.key)
                    .count(),
                1,
                "{}",
                entry.key
            );
        }
    }

    #[test]
    fn a_taken_name_gets_the_lowest_free_number() {
        assert_eq!(unique_name("Cube", &["Cube", "Cube 2", "Cube 4"]), "Cube 3");
        assert_eq!(unique_name("Cube", &[]), "Cube");
    }
}
