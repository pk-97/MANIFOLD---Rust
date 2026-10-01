//! The liquid an Add Fluid inserts, as data the caller hands the command.

use std::collections::BTreeMap;

use manifold_core::effect_graph_def::{
    EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, SerializedParamValue,
};

use super::{
    MATERIAL_TYPE_ID, ROLE_SOURCE_TYPE_ID, SCENE_OBJECT_TYPE_ID, TRANSFORM_TYPE_ID, float, int,
    scene_build_node, scene_build_wire,
};

/// Which of the command's metadata lists stamps a template node's card rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExposureSet {
    Fluid,
    Domain,
    SourceTransform,
    Role,
    Material,
    Object,
}

/// One node's card rows. `section` is the suffix after `"<Fluid N> - "`;
/// `None` uses the bare fluid handle.
#[derive(Clone, Debug)]
pub struct TemplateExposure {
    pub node: u32,
    pub set: ExposureSet,
    pub section: Option<&'static str>,
}

/// A self-contained group body that ends in a `system.group_output` node
/// fed an `object` port. Node ids are local to the template; the command
/// gives every node, nested ones included, a fresh document id on insert.
///
/// A node's `handle` is a suffix: `Some("Simulation")` becomes
/// `"<Fluid N> Simulation"`, `Some("")` is the bare fluid handle, `None`
/// carries no handle. A node's `node_id` is the stable-id prefix.
///
/// The body holds exactly one liquid domain, at any group depth. The command
/// finds it by `liquid_domains_in` and wires the shared World controls
/// (gravity, speed, reset) to it through every group boundary on the way.
#[derive(Clone, Debug)]
pub struct LiquidTemplate {
    pub nodes: Vec<EffectGraphNode>,
    pub wires: Vec<EffectGraphWire>,
    pub output_node: u32,
    /// How many body nodes get document ids before the group node does. Flows
    /// address card params by document id, so FLIP keeps its original order.
    pub group_id_slot: usize,
    pub exposures: Vec<TemplateExposure>,
}

impl LiquidTemplate {
    /// Node type of the node whose card rows `set` stamps, so the caller can
    /// look up that type's metadata.
    pub fn exposed_type_id(&self, set: ExposureSet) -> Option<&str> {
        let exposure = self.exposures.iter().find(|exposure| exposure.set == set)?;
        let node = self.nodes.iter().find(|node| node.id == exposure.node)?;
        Some(node.type_id.as_str())
    }
}

fn template_node(
    id: u32,
    prefix: &str,
    type_id: &str,
    handle: Option<&str>,
    params: BTreeMap<String, SerializedParamValue>,
) -> EffectGraphNode {
    let mut node = scene_build_node(id, type_id, handle.map(str::to_owned), params);
    node.node_id = manifold_core::NodeId::new(prefix);
    node
}

fn params(entries: &[(&str, SerializedParamValue)]) -> BTreeMap<String, SerializedParamValue> {
    entries.iter().map(|(name, value)| ((*name).to_owned(), value.clone())).collect()
}

/// Today's FLIP scene fluid: a CPU FLIP domain fed by one emitter role.
pub fn flip_scene_fluid_template() -> LiquidTemplate {
    const FLUID: u32 = 1;
    const SOURCE: u32 = 2;
    const ROLE: u32 = 3;
    const DOMAIN: u32 = 4;
    const MATERIAL: u32 = 5;
    const OBJECT: u32 = 6;
    const OUTPUT: u32 = 7;

    let fluid = template_node(
        FLUID,
        "fluid_surface",
        manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID,
        Some("Simulation"),
        params(&[
            ("domain_size", float(4.0)),
            ("fill_height", float(0.4)),
            ("resolution", int(16)),
            ("whitewater", float(0.0)),
            ("gravity", float(-9.81)),
            ("emission", float(0.0)),
            ("inflow_speed", float(1.0)),
            ("speed", float(1.0)),
            ("surface_subdivisions", int(0)),
        ]),
    );
    let source = template_node(
        SOURCE,
        "fluid_source",
        TRANSFORM_TYPE_ID,
        Some("Source"),
        params(&[
            ("pos_x", float(0.0)),
            ("pos_y", float(2.8)),
            ("pos_z", float(0.0)),
            ("rot_x", float(0.0)),
            ("rot_y", float(0.0)),
            ("rot_z", float(0.0)),
            ("scale_x", float(0.7)),
            ("scale_y", float(0.5)),
            ("scale_z", float(0.7)),
        ]),
    );
    let role = template_node(
        ROLE,
        "fluid_role_source",
        ROLE_SOURCE_TYPE_ID,
        Some("Source Role"),
        params(&[
            ("role", SerializedParamValue::Enum { value: 1 }),
            ("enabled", SerializedParamValue::Bool { value: true }),
            ("geometry", SerializedParamValue::Enum { value: 0 }),
            ("shape", SerializedParamValue::Enum { value: 1 }),
            ("radius", float(3.0_f32.sqrt() / 2.0)),
            ("velocity_x", float(0.0)),
            ("velocity_y", float(-1.0)),
            ("velocity_z", float(0.0)),
            ("inherit_motion", float(0.0)),
            ("friction", float(0.0)),
            ("collider_parts", int(32)),
        ]),
    );
    let domain = template_node(
        DOMAIN,
        "fluid_domain",
        TRANSFORM_TYPE_ID,
        Some("Domain"),
        params(&[
            ("pos_x", float(0.0)),
            ("pos_y", float(2.0)),
            ("pos_z", float(0.0)),
            ("rot_x", float(0.0)),
            ("rot_y", float(0.0)),
            ("rot_z", float(0.0)),
            ("scale_x", float(4.0)),
            ("scale_y", float(4.0)),
            ("scale_z", float(4.0)),
        ]),
    );
    let material = template_node(
        MATERIAL,
        "fluid_material",
        MATERIAL_TYPE_ID,
        Some("Material"),
        params(&[
            ("color_r", float(0.8)),
            ("color_g", float(0.95)),
            ("color_b", float(1.0)),
            ("roughness", float(0.08)),
            ("transmission", float(1.0)),
            ("ior", float(1.333)),
            ("volume_geometry", float(1.0)),
            ("volume_attenuation_color_r", float(0.6)),
            ("volume_attenuation_color_g", float(0.85)),
            ("volume_attenuation_color_b", float(0.95)),
            ("volume_attenuation_distance", float(2.0)),
        ]),
    );
    let object = template_node(OBJECT, "fluid_object", SCENE_OBJECT_TYPE_ID, Some(""), BTreeMap::new());
    let output = template_node(OUTPUT, "fluid_output", GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());

    LiquidTemplate {
        nodes: vec![fluid, source, material, object, output, role, domain],
        group_id_slot: 5,
        wires: vec![
            scene_build_wire(SOURCE, "transform", ROLE, "transform"),
            scene_build_wire(DOMAIN, "transform", FLUID, "domain"),
            scene_build_wire(ROLE, "role", FLUID, "role_0"),
            scene_build_wire(FLUID, "vertices", OBJECT, "vertices"),
            scene_build_wire(MATERIAL, "out", OBJECT, "material"),
            scene_build_wire(OBJECT, "object", OUTPUT, "object"),
        ],
        output_node: OUTPUT,
        exposures: vec![
            TemplateExposure { node: FLUID, set: ExposureSet::Fluid, section: Some("Simulation") },
            TemplateExposure { node: DOMAIN, set: ExposureSet::Domain, section: Some("Domain") },
            TemplateExposure { node: SOURCE, set: ExposureSet::SourceTransform, section: Some("Source Transform") },
            TemplateExposure { node: ROLE, set: ExposureSet::Role, section: Some("Source") },
            TemplateExposure { node: MATERIAL, set: ExposureSet::Material, section: Some("Material") },
            TemplateExposure { node: OBJECT, set: ExposureSet::Object, section: None },
        ],
    }
}
