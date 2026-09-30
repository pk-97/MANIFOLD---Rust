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

/// Every Resolution the matter presets allow keeps its reads inside the grid.
#[test]
fn face_grid_extents_at_every_matter_resolution() {
    for res in 8..=512 {
        check_matter(&MatterLattice::from_layout(&domain_layout(None, 4.0, res).expect("layout")));
    }
    assert_eq!(matter_cells([7, 20, 20]), None, "a lattice with no cells is refused");
}
