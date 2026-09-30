//! CPU proof that the face grid's arrays hold everything their dispatches
//! read and write (GPU_WHITEWATER_DESIGN.md I9: an extent proof precedes
//! every new GPU size), at 64 and at every Resolution the matter presets
//! allow. No GPU: every size comes from the functions the atoms size and
//! dispatch with.

use super::face_sample_component::FaceSampleComponent;
use super::matter_face_component::{MatterFaceComponent, matter_cells};
use super::particles_to_faces::face_count;
use crate::node_graph::effect_node::ParamValues;
use crate::node_graph::fluid::domain_layout;
use crate::node_graph::liquid::grid::{face_dims, face_len};
use crate::node_graph::matter::{MatterLattice, PADDING_NODES, lattice_nodes};
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitive::Primitive;

fn lattice_params(cells: [u32; 3], axis: u32) -> ParamValues {
    let mut params = ParamValues::default();
    params.insert("axis".into(), ParamValue::Enum(axis));
    for (name, n) in ["nodes_x", "nodes_y", "nodes_z"].into_iter().zip(cells) {
        params.insert(name.into(), ParamValue::Float(n as f32));
    }
    params
}

/// The last FaceSample record the SWASH component reads for `axis`: its
/// last face, a padded cell of the (n+1)³ lattice.
fn swash_last_read(cells: [u32; 3], axis: usize) -> u64 {
    let last = face_dims(cells, axis).map(|d| u64::from(d) - 1);
    let m = cells.map(|n| u64::from(n) + 1);
    last[0] + m[0] * (last[1] + m[1] * last[2])
}

/// The last grid node the matter component reads for `axis`: its last face
/// plus the padding, and one node on along the other two axes.
fn matter_last_read(nodes: [u32; 3], axis: usize) -> u64 {
    let cells = matter_cells(nodes).expect("a lattice with cells");
    let last = face_dims(cells, axis).map(|d| u64::from(d) - 1);
    let q: [u64; 3] = std::array::from_fn(|b| last[b] + u64::from(PADDING_NODES) + u64::from(b != axis));
    let n = nodes.map(u64::from);
    q[0] + n[0] * (q[1] + n[1] * q[2])
}

fn check_swash(cells: [u32; 3]) {
    for axis in 0..3 {
        let count = face_len(cells, axis);
        let capacity = FaceSampleComponent::new()
            .array_output_capacity("out", &lattice_params(cells, axis as u32), &[])
            .expect("out capacity");
        assert_eq!(u64::from(capacity), count, "{cells:?} axis {axis}: out holds exactly the axis's faces");
        assert!(swash_last_read(cells, axis) < face_count(cells), "{cells:?} axis {axis}: read past the lattice");
        assert!(count <= u64::from(u32::MAX), "{cells:?} axis {axis}: dispatch count");
    }
}

fn check_matter(lattice: &MatterLattice) {
    let nodes = lattice_nodes(lattice.nodes);
    assert_eq!(matter_cells(lattice.nodes), Some(lattice.cells), "{:?}", lattice.nodes);
    for axis in 0..3 {
        let count = face_len(lattice.cells, axis);
        let capacity = MatterFaceComponent::new()
            .array_output_capacity("out", &ParamValues::default(), &[("grid", nodes as u32)])
            .expect("out capacity");
        assert!(count <= u64::from(capacity), "{:?} axis {axis}: out is shorter than the faces", lattice.nodes);
        assert!(matter_last_read(lattice.nodes, axis) < nodes, "{:?} axis {axis}: read past the grid", lattice.nodes);
    }
}

/// At 64, the size every GPU run of this design uses: 266,240 faces per
/// axis, 1.07 MB per array.
#[test]
fn face_grid_extents_at_64() {
    check_swash([64; 3]);
    check_swash([6, 5, 4]);
    let lattice = MatterLattice::from_layout(&domain_layout(None, 4.0, 64).expect("layout"));
    assert_eq!(lattice.cells, [64; 3]);
    check_matter(&lattice);
    assert_eq!(face_len([64; 3], 0), 266_240);
    assert_eq!(face_len([64; 3], 0) * 4, 1_064_960);
}

/// Where the face grid nodes fuse in their host graphs. SWASH's component
/// sizes its output from params, so the freeze compiler refuses it even
/// beside a coincident consumer (BUG-u8io, param-sized outputs never fuse).
/// The matter component sizes its output from its grid input, so it folds
/// into one region with its consumer and stands alone without one; the
/// fused-vs-unfused GPU proof covers the folded case.
#[test]
fn face_grid_fusion_in_host_graphs() {
    use super::face_grid_scenes::matter_dam_break_faces;
    use super::swash_preset::{FACE_NODES, WaterScene, water_def};
    use crate::node_graph::FusionReport;
    let mut registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    crate::node_graph::substeps::test_nodes::register_substep_test_nodes(&mut registry);
    let report = |def| {
        let report = crate::node_graph::fusion_report(&def, &registry);
        assert!(report.preparation_error.is_none(), "{:?}", report.preparation_error);
        report
    };
    let of_type = |report: &FusionReport, type_id: &str| -> Vec<_> { report.nodes.iter().filter(|n| n.type_id == type_id).cloned().collect() };

    let mut swash = serde_json::to_value(water_def(WaterScene::dam_break(64).with_faces())).expect("def");
    let nodes = swash["nodes"].as_array().expect("nodes");
    let face_u = nodes.iter().find(|n| n["nodeId"] == FACE_NODES[0]).expect("face_u")["id"].clone();
    let next = nodes.iter().filter_map(|n| n["id"].as_u64()).max().expect("ids") + 1;
    swash["nodes"].as_array_mut().expect("nodes").push(serde_json::json!({
        "id": next, "nodeId": "face_u_consumer", "typeId": "node.cosine_poisson_divide", "params": {},
    }));
    swash["wires"].as_array_mut().expect("wires").push(serde_json::json!({"fromNode": face_u, "fromPort": "out", "toNode": next, "toPort": "values"}));
    let swash = report(serde_json::from_value(swash).expect("def"));
    let sampled = of_type(&swash, "node.face_sample_component");
    assert_eq!(sampled.len(), 3);
    assert!(sampled.iter().all(|n| !n.fused), "SWASH face components stay unfused: {sampled:?}");

    let alone = report(matter_dam_break_faces(None, false));
    let components = of_type(&alone, "node.matter_face_component");
    assert_eq!(components.len(), 3);
    assert!(components.iter().all(|n| !n.fused), "a lone matter component is its own dispatch: {components:?}");

    let consumed = report(matter_dam_break_faces(Some(1), false));
    let components = of_type(&consumed, "node.matter_face_component");
    let fused: Vec<_> = components.iter().filter(|n| n.fused).collect();
    assert_eq!(fused.len(), 1, "only the consumed axis fuses: {components:?}");
    let region = &consumed.regions[fused[0].region_index.expect("region")];
    let members: Vec<_> = consumed.nodes.iter().filter(|n| region.member_node_ids.contains(&n.node_id)).map(|n| n.type_id.as_str()).collect();
    assert_eq!(members.len(), 2, "component and consumer only: {members:?}");
    assert!(members.contains(&"node.cosine_poisson_divide"), "the consumer shares the region: {members:?}");
}

/// I2: whitewater reads face velocity at least one layer past the liquid.
/// SWASH extends two; MPM's faces carry none past the liquid yet, so an MPM
/// face grid is refused by name (GPU_WHITEWATER_DESIGN.md section 3.6).
#[test]
fn whitewater_refuses_unextended_faces() {
    use crate::node_graph::whitewater::require_extended_faces;
    assert!(require_extended_faces(super::swash_preset::EXTENDED_LAYERS as f32).is_ok());
    assert!(require_extended_faces(1.0).is_ok());
    let layers = super::matter_face_component::MATTER_FACE_VALID_LAYERS as f32;
    assert!(require_extended_faces(layers).expect_err("MPM refused").contains("needs at least 1"));
}

/// Every Resolution the matter presets allow keeps its reads inside the grid.
#[test]
fn face_grid_extents_at_every_matter_resolution() {
    for res in 8..=512 {
        check_matter(&MatterLattice::from_layout(&domain_layout(None, 4.0, res).expect("layout")));
    }
    assert_eq!(matter_cells([7, 20, 20]), None, "a lattice with no cells is refused");
}
