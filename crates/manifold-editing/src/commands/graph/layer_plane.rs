//! Scene-build command for a layer plane — the add-a-plane gesture
//! (BUG-4gba): one undoable composite edit that (1) bumps
//! `render_scene`'s `objects` count by one, (2) builds a new group named
//! "Layer Plane N" containing a `node.plane_mesh` (width/height from the
//! caller) + a Mask/cutout `node.unlit_material` + a `node.transform_3d` +
//! an EMPTY `node.layer_source` ("Skin", wired to `base_color_map`) + a
//! `node.scene_object`, wired to a `system.group_output` boundary exposing
//! the object, (3) wires the group's `object` output to the new
//! `object_k` port on `render_scene`. Undo restores the pre-edit
//! `(nodes, wires, preset_metadata)` verbatim — the same whole-level
//! snapshot shape `AddSceneObjectCommand` uses.
//!
//! The layer plane is the "video on a sheet in 3D" gesture: the empty
//! `layer` param on the skin renders transparent black until the user
//! picks a source in the scene panel's Skin row (which discovers a
//! `node.layer_source` wired to `base_color_map` — no new UI). The caller
//! (app-side) computes width/height from the canvas aspect; the command
//! just takes the two f32s.
//!
//! `next_index` (the new object's 0-based slot, `k` in `object_k`) is
//! resolved by the caller from the LIVE `objects` param value shown on the
//! node face at click time — not re-derived here, same posture
//! `AddSceneObjectCommand` documents.

use std::collections::BTreeMap;

use manifold_core::GraphTarget;
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID,
    GroupDef, GroupInterface, InterfacePortDef, PresetMetadata, SerializedParamValue,
};
use manifold_core::project::Project;
use manifold_core::scene_exposure::{stamp_scene_node_exposures_into, SceneParamMetadata};

use crate::command::Command;

use super::{
    descend_level, refresh_target_manifest, scene_build_node, scene_build_wire,
    with_existing_target_graph_mut, with_target_graph_mut,
};

/// The add-layer-plane gesture (BUG-4gba). Mirrors
/// [`AddSceneObjectCommand`] (same target/scope/render-scene/index/
/// centroid + P1 metadata + catalog default + whole-level snapshot undo),
/// with two divergences: the mesh is a `node.plane_mesh` sized by
/// `width`/`height` (port-shadowed scalars, set as params here), and the
/// group carries a `node.layer_source` ("Skin") wired to the
/// `scene_object`'s `base_color_map` — the layer-skin pairing so the
/// plane can wear another layer's live output.
#[derive(Debug)]
pub struct AddSceneLayerPlaneCommand {
    target: GraphTarget,
    scope_path: Vec<u32>,
    render_scene_node_id: u32,
    next_index: u32,
    centroid: (f32, f32),
    /// Plane size in world units. The app-side caller computes these from
    /// the canvas aspect (`width = height * aspect`) so a skinned layer
    /// composite is undistorted on the sheet.
    width: f32,
    height: f32,
    /// P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the new material/
    /// transform/scene_object nodes' full param manifests, computed by the
    /// app-side caller via `manifold_renderer::node_graph::scene_exposure::
    /// metadata_for_node_type` (this crate has no renderer dep) — `execute`
    /// stamps them into the def's top-level `preset_metadata` after minting
    /// the new nodes' ids.
    material_metadata: Vec<SceneParamMetadata>,
    transform_metadata: Vec<SceneParamMetadata>,
    scene_object_metadata: Vec<SceneParamMetadata>,
    catalog_default: EffectGraphDef,
    /// The level's `(nodes, wires)` before this edit, plus the pre-edit
    /// whole-def `preset_metadata` (P1 exposure stamping lands there, outside
    /// the scoped level). Set on execute.
    prev: Option<(Vec<EffectGraphNode>, Vec<EffectGraphWire>, Option<PresetMetadata>)>,
}

impl AddSceneLayerPlaneCommand {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: GraphTarget,
        scope_path: Vec<u32>,
        render_scene_node_id: u32,
        next_index: u32,
        centroid: (f32, f32),
        width: f32,
        height: f32,
        material_metadata: Vec<SceneParamMetadata>,
        transform_metadata: Vec<SceneParamMetadata>,
        scene_object_metadata: Vec<SceneParamMetadata>,
        catalog_default: EffectGraphDef,
    ) -> Self {
        Self {
            target,
            scope_path,
            render_scene_node_id,
            next_index,
            centroid,
            width,
            height,
            material_metadata,
            transform_metadata,
            scene_object_metadata,
            catalog_default,
            prev: None,
        }
    }
}

/// A distinct RGBA tint for layer-plane slot `k` — the SAME golden-ratio
/// hue-spread formula `scene.rs::scene_object_tint` uses for added cubes,
/// so a layer plane reads as one more colour-coded object beside imported
/// ones. (That fn is private to `scene.rs`; this is a same-formula
/// re-derivation, keep the two in sync — see the note there.)
fn layer_plane_tint(k: u32) -> manifold_core::Color {
    let hue = (k as f32 * 0.618_034) % 1.0;
    manifold_core::Color::hsv_to_rgb(hue, 0.7, 0.85)
}

impl Command for AddSceneLayerPlaneCommand {
    fn execute(&mut self, project: &mut Project) {
        let scope = self.scope_path.clone();
        let render_id = self.render_scene_node_id;
        let k = self.next_index;
        let centroid = self.centroid;
        let width = self.width;
        let height = self.height;
        let result = with_target_graph_mut(project, &self.target, &self.catalog_default, true, |def| {
            let prev_metadata = def.preset_metadata.clone();

            // Build the group + wire it in, entirely within a nested block so
            // the `nodes`/`wires` borrows (from `descend_level`) end before
            // the P1 exposure stamping below touches `def.preset_metadata` —
            // same "metadata vs. nodes/wires never overlap" discipline
            // `AddSceneObjectCommand` documents.
            let (mat_id, mat_node_id, mat_node_params, transform_id, transform_node_id, scene_object_id, scene_object_node_id, handle, prev) = {
                let (nodes, wires) = descend_level(&mut def.nodes, &mut def.wires, &scope)?;
                let prev = (nodes.clone(), wires.clone());

                nodes
                    .iter_mut()
                    .find(|n| n.id == render_id)?
                    .params
                    .insert(
                        "objects".to_string(),
                        SerializedParamValue::Float {
                            value: (k + 1) as f32,
                        },
                    );

                let mut next_id = nodes.iter().map(|n| n.id).max().map_or(0, |m| m + 1);
                let mut fresh = move || {
                    let v = next_id;
                    next_id += 1;
                    v
                };
                let plane_id = fresh();
                let mat_id = fresh();
                let transform_id = fresh();
                let skin_id = fresh();
                let scene_object_id = fresh();
                let out_id = fresh();
                let group_id = fresh();

                let tint = layer_plane_tint(k);

                let mut plane_params = BTreeMap::new();
                plane_params.insert(
                    "width".to_string(),
                    SerializedParamValue::Float { value: width },
                );
                plane_params.insert(
                    "height".to_string(),
                    SerializedParamValue::Float { value: height },
                );

                let plane_node = scene_build_node(
                    plane_id,
                    "node.plane_mesh",
                    Some(format!("plane_{k}")),
                    plane_params,
                );
                // unlit_material alpha_mode is an ENUM: 0=Opaque, 1=Mask,
                // 2=Blend (transcribed from `unlit_material.rs`'s
                // `ALPHA_MODES` — the primitive's own defaults are Opaque,
                // so the cutout is stamped explicitly here; a skinned
                // transparent-black backing must not block the sheet's
                // alpha once the layer has one).
                let mat_node = scene_build_node(
                    mat_id,
                    "node.unlit_material",
                    Some(format!("mat_{k}")),
                    BTreeMap::from([
                        (
                            "alpha_mode".to_string(),
                            SerializedParamValue::Enum { value: 1 }, // Mask
                        ),
                        (
                            "color_r".to_string(),
                            SerializedParamValue::Float { value: tint.r },
                        ),
                        (
                            "color_g".to_string(),
                            SerializedParamValue::Float { value: tint.g },
                        ),
                        (
                            "color_b".to_string(),
                            SerializedParamValue::Float { value: tint.b },
                        ),
                    ]),
                );
                let mat_node_id = mat_node.node_id.clone();
                let mat_node_params = mat_node.params.clone();
                let transform_node = scene_build_node(
                    transform_id,
                    "node.transform_3d",
                    Some(format!("transform_{k}")),
                    BTreeMap::new(),
                );
                let transform_node_id = transform_node.node_id.clone();
                // Empty `layer` — transparent black from the skin until the
                // user picks a source layer in the scene panel's Skin row.
                let skin_node = scene_build_node(
                    skin_id,
                    "node.layer_source",
                    Some("Skin".to_string()),
                    BTreeMap::from([(
                        "layer".to_string(),
                        SerializedParamValue::String { value: String::new() },
                    )]),
                );
                let handle = format!("Layer Plane {}", k + 1);
                let scene_object_node =
                    scene_build_node(scene_object_id, "node.scene_object", Some(handle.clone()), BTreeMap::new());
                let scene_object_node_id = scene_object_node.node_id.clone();
                let out_node = scene_build_node(out_id, GROUP_OUTPUT_TYPE_ID, None, BTreeMap::new());

                let group_wires = vec![
                    scene_build_wire(plane_id, "vertices", scene_object_id, "vertices"),
                    scene_build_wire(mat_id, "out", scene_object_id, "material"),
                    scene_build_wire(transform_id, "transform", scene_object_id, "transform"),
                    scene_build_wire(skin_id, "out", scene_object_id, "base_color_map"),
                    scene_build_wire(scene_object_id, "object", out_id, "object"),
                ];

                let mut group_node =
                    scene_build_node(group_id, GROUP_TYPE_ID, Some(handle.clone()), BTreeMap::new());
                group_node.editor_pos = Some(centroid);
                group_node.group = Some(Box::new(GroupDef {
                    interface: GroupInterface {
                        inputs: Vec::new(),
                        outputs: vec![InterfacePortDef {
                            name: "object".to_string(),
                            port_type: "Object".to_string(),
                        }],
                        params: Vec::new(),
                    },
                    nodes: vec![plane_node, mat_node, transform_node, skin_node, scene_object_node, out_node],
                    wires: group_wires,
                    tint: Some([tint.r, tint.g, tint.b, 1.0]),
                }));

                nodes.push(group_node);
                wires.push(scene_build_wire(group_id, "object", render_id, &format!("object_{k}")));

                (
                    mat_id,
                    mat_node_id,
                    mat_node_params,
                    transform_id,
                    transform_node_id,
                    scene_object_id,
                    scene_object_node_id,
                    handle,
                    prev,
                )
            };

            // P1 (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): expose every
            // param of the freshly minted material/transform/scene_object
            // nodes, into the def's TOP-LEVEL preset_metadata, targeting each
            // node's bare NodeId — same convention `AddSceneObjectCommand`
            // uses. The layer plane's scene_object section is the handle
            // ("Layer Plane N") and its material section is
            // "{handle} — Material", matching the object gesture.
            let meta = def.preset_metadata.get_or_insert_with(|| PresetMetadata {
                id: manifold_core::PresetTypeId::from_string("UnnamedScene".to_string()),
                display_name: "Scene".to_string(),
                category: "Geometry".to_string(),
                osc_prefix: "scene".to_string(),
                legacy_discriminant: None,
                available: true,
                is_line_based: false,
                params: Vec::new(),
                bindings: Vec::new(),
                param_aliases: Vec::new(),
                value_aliases: Vec::new(),
                string_params: Vec::new(),
                string_bindings: Vec::new(),
                scene_bounds: None,
            });
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                mat_id,
                &mat_node_id,
                "node.unlit_material",
                &format!("{handle} — Material"),
                &self.material_metadata,
                &mat_node_params,
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                transform_id,
                &transform_node_id,
                "node.transform_3d",
                &format!("{handle} — Transform"),
                &self.transform_metadata,
                &BTreeMap::new(),
            );
            stamp_scene_node_exposures_into(
                &mut meta.params,
                &mut meta.bindings,
                scene_object_id,
                &scene_object_node_id,
                "node.scene_object",
                &handle,
                &self.scene_object_metadata,
                &BTreeMap::new(),
            );

            Some((prev, prev_metadata))
        });
        if let Some((pnw, pmeta)) = result.flatten() {
            self.prev = Some((pnw.0, pnw.1, pmeta));
        }
        refresh_target_manifest(project, &self.target);
    }

    fn undo(&mut self, project: &mut Project) {
        let Some((pn, pw, pmeta)) = self.prev.clone() else {
            return;
        };
        let scope = self.scope_path.clone();
        let _ = with_existing_target_graph_mut(project, &self.target, true, |def| {
            def.preset_metadata = pmeta;
            if let Some((nodes, wires)) = descend_level(&mut def.nodes, &mut def.wires, &scope) {
                *nodes = pn;
                *wires = pw;
            }
        });
        refresh_target_manifest(project, &self.target);
    }

    fn description(&self) -> &str {
        "Add Layer Plane"
    }
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{mirror_catalog_default, project_with_one_generator_layer};
    use super::*;
    use manifold_core::effect_graph_def::{EFFECT_GRAPH_VERSION, GROUP_TYPE_ID};
    use manifold_core::LayerId;

    /// A single `node.render_scene` node (id 0) with `objects` set to the
    /// given count — the fixture `AddSceneLayerPlaneCommand` operates
    /// against.
    fn render_scene_graph(objects: u32) -> EffectGraphDef {
        let mut render = EffectGraphNode {
            id: 0,
            node_id: manifold_core::NodeId::new("render"),
            type_id: "node.render_scene".to_string(),
            handle: Some("render".to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: BTreeMap::new(),
            output_canvas_scales: BTreeMap::new(),
            group: None,
        };
        render
            .params
            .insert("objects".to_string(), SerializedParamValue::Float { value: objects as f32 });
        EffectGraphDef {
            version: EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render],
            wires: vec![],
        }
    }

    /// A generator-hosted project (the production target shape —
    /// `AddSceneObjectCommand`'s tests document why generator targets are
    /// the realistic fixture for manifest stamping).
    fn generator_project(def: EffectGraphDef) -> (Project, LayerId) {
        let (mut project, lid) = project_with_one_generator_layer();
        let target = GraphTarget::Generator(lid.clone());
        with_target_graph_mut(&mut project, &target, &def, true, |_| Some(())).unwrap();
        (project, lid)
    }

    fn def_of<'p>(project: &'p Project, lid: &LayerId) -> &'p EffectGraphDef {
        project
            .timeline
            .find_layer_by_id(lid)
            .unwrap()
            .1
            .gen_params()
            .unwrap()
            .graph
            .as_ref()
            .unwrap()
    }

    #[test]
    fn add_layer_plane_builds_group_with_skin_and_undo_restores() {
        let (mut project, lid) = generator_project(render_scene_graph(2));
        let before = def_of(&project, &lid).clone();

        let mut cmd = AddSceneLayerPlaneCommand::new(
            GraphTarget::Generator(lid.clone()),
            vec![],
            0,
            2, // next_index — matches the current `objects` (2)
            (100.0, 200.0),
            1.6, // width
            0.9, // height
            vec![super::super::test_support::scene_param_meta("color_r", "Colour R")],
            vec![super::super::test_support::scene_param_meta("pos_x", "X")],
            vec![super::super::test_support::scene_param_meta("visible", "Visible")],
            mirror_catalog_default(),
        );
        cmd.execute(&mut project);

        let def = def_of(&project, &lid);
        let render = def.nodes.iter().find(|n| n.id == 0).unwrap();
        assert_eq!(
            render.params.get("objects"),
            Some(&SerializedParamValue::Float { value: 3.0 }),
            "objects bumped by one"
        );

        let group = def
            .nodes
            .iter()
            .find(|n| n.handle.as_deref() == Some("Layer Plane 3"))
            .expect("named group created");
        assert_eq!(group.editor_pos, Some((100.0, 200.0)));
        assert_eq!(group.type_id, GROUP_TYPE_ID);
        let body = group.group.as_deref().expect("is a group node");
        assert_eq!(
            body.nodes.len(),
            6,
            "plane + material + transform + layer_source + scene_object + group_output boundary"
        );

        let plane = body.nodes.iter().find(|n| n.type_id == "node.plane_mesh").expect("plane node");
        assert_eq!(plane.params.get("width"), Some(&SerializedParamValue::Float { value: 1.6 }));
        assert_eq!(plane.params.get("height"), Some(&SerializedParamValue::Float { value: 0.9 }));

        let mat = body.nodes.iter().find(|n| n.type_id == "node.unlit_material").expect("material node");
        assert_eq!(
            mat.params.get("alpha_mode"),
            Some(&SerializedParamValue::Enum { value: 1 }),
            "alpha_mode stamped to Mask (cutout)"
        );

        let skin = body.nodes.iter().find(|n| n.type_id == "node.layer_source").expect("skin node");
        assert_eq!(skin.handle.as_deref(), Some("Skin"));
        assert_eq!(
            skin.params.get("layer"),
            Some(&SerializedParamValue::String { value: String::new() }),
            "empty layer param — transparent black until a source is picked"
        );

        assert!(body.nodes.iter().any(|n| n.type_id == "node.transform_3d"));
        assert!(body.nodes.iter().any(|n| n.type_id == "node.scene_object"));
        assert!(body.nodes.iter().any(|n| n.type_id == GROUP_OUTPUT_TYPE_ID));

        // Five internal wires: plane→vertices, material→material,
        // transform→transform, layer_source→base_color_map, scene_object→out.
        assert_eq!(body.wires.len(), 5);
        let scene_object_id = body
            .nodes
            .iter()
            .find(|n| n.type_id == "node.scene_object")
            .unwrap()
            .id;
        let plane_id = plane.id;
        let mat_id = mat.id;
        let skin_id = skin.id;
        assert!(body.wires.iter().any(|w| w.from_node == plane_id
            && w.from_port == "vertices" && w.to_node == scene_object_id && w.to_port == "vertices"));
        assert!(body.wires.iter().any(|w| w.from_node == mat_id
            && w.from_port == "out" && w.to_node == scene_object_id && w.to_port == "material"));
        assert!(body.wires.iter().any(|w| w.from_node == skin_id
            && w.from_port == "out" && w.to_node == scene_object_id && w.to_port == "base_color_map"));
        assert!(body.wires.iter().any(|w| w.from_node == scene_object_id
            && w.from_port == "object"));

        assert_eq!(body.interface.outputs.len(), 1, "a single Object output");
        assert_eq!(body.interface.outputs[0].name, "object");
        assert_eq!(body.interface.outputs[0].port_type, "Object");

        // The group's single `object` output wired to render_scene's new
        // object_2 slot.
        assert!(def.wires.iter().any(|w| w.from_node == group.id
            && w.from_port == "object"
            && w.to_node == 0
            && w.to_port == "object_2"));

        cmd.undo(&mut project);
        assert_eq!(def_of(&project, &lid), &before, "undo restores the pre-add graph exactly");
    }

    /// P1: the command stamps material/transform/scene_object metadata into
    /// the def's TOP-LEVEL `preset_metadata`, sectioned per the object
    /// convention ("Layer Plane N — Material" / "… — Transform" / handle).
    /// Undo restores `preset_metadata` verbatim.
    #[test]
    fn add_layer_plane_stamps_exposures_and_undo_restores_them() {
        use manifold_core::effect_graph_def::BindingTarget;

        let (mut project, lid) = generator_project(render_scene_graph(0));

        let mut cmd = AddSceneLayerPlaneCommand::new(
            GraphTarget::Generator(lid.clone()),
            vec![],
            0,
            0,
            (0.0, 0.0),
            1.0,
            1.0,
            vec![super::super::test_support::scene_param_meta("color_r", "Colour R")],
            vec![super::super::test_support::scene_param_meta("pos_x", "X")],
            vec![super::super::test_support::scene_param_meta("visible", "Visible")],
            mirror_catalog_default(),
        );

        let assert_stamped = |project: &Project| {
            let def = def_of(project, &lid);
            let group = def.nodes.iter().find(|n| n.handle.as_deref() == Some("Layer Plane 1")).unwrap();
            let body = group.group.as_deref().unwrap();
            let mat_node = body.nodes.iter().find(|n| n.type_id == "node.unlit_material").unwrap();
            let transform_node = body.nodes.iter().find(|n| n.type_id == "node.transform_3d").unwrap();
            let scene_object_node = body.nodes.iter().find(|n| n.type_id == "node.scene_object").unwrap();

            let meta = def.preset_metadata.as_ref().expect("P1 stamped into top-level preset_metadata");
            assert_eq!(meta.params.len(), 3, "one ParamSpecDef per exposed param");
            assert_eq!(meta.bindings.len(), 3);

            let has_binding = |node_id: &manifold_core::NodeId, param: &str, section: &str| {
                meta.bindings.iter().any(|b| {
                    matches!(&b.target, BindingTarget::Node { node_id: nid, param: p } if nid == node_id && p == param)
                }) && meta.params.iter().any(|p| p.section.as_deref() == Some(section))
            };
            assert!(
                has_binding(&mat_node.node_id, "color_r", "Layer Plane 1 — Material"),
                "material exposure targets the grouped node's bare NodeId, section 'Layer Plane 1 — Material'"
            );
            assert!(
                has_binding(&transform_node.node_id, "pos_x", "Layer Plane 1 — Transform"),
                "transform exposure targets the grouped node's bare NodeId"
            );
            assert!(
                has_binding(&scene_object_node.node_id, "visible", "Layer Plane 1"),
                "scene_object exposure targets the grouped node's bare NodeId, section 'Layer Plane 1'"
            );
        };

        cmd.execute(&mut project);
        assert_stamped(&project);

        cmd.undo(&mut project);
        assert!(def_of(&project, &lid).preset_metadata.is_none(), "undo restores the pre-add (empty) preset_metadata verbatim");

        cmd.execute(&mut project); // redo
        assert_stamped(&project);
    }
}