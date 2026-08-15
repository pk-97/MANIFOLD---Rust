//! Who wins when a def bakes a value onto a node an outer card also drives.
//!
//! One rule, two answers, and the answer is the binding's provenance:
//!
//! - a MIRRORED default (`default_mirrors_node_param`, set only by
//!   `stamp_scene_node_exposures_into`) is a stamp-time snapshot of the node
//!   param, so `apply_binding_defaults` skips planting it and the node value
//!   stands — BUG-ji6q, an imported scene keeping the values you set;
//! - an AUTHORED default is a number someone chose, and the plant is where its
//!   `scale`/`offset` fold is applied, so it still overwrites whatever the def
//!   baked — silently, which is BUG-1l7f.
//!
//! Its own module because the pair is a precedence rule, not glTF trivia, and
//! because `tests.rs` is at its god-file ceiling (BUG-adpu, gltf_import
//! decomposition). The unit-level half of the same rule lives beside the
//! detector in `node_graph/bound_graph.rs`, where the `Graph` + binding harness
//! it needs already is; this file is the import-level proof, on a def a real GLB
//! import produces.

use crate::node_graph::gltf_load::GltfImportSummary;

use super::scene::build_import_graph;
use super::tests::full_material;
use crate::node_graph::PrimitiveRegistry;
use crate::preset_runtime::PresetRuntime;

/// BUG-1l7f, the part of the imported-def footgun BUG-ji6q did NOT close.
///
/// A scene-vocabulary param (`transform_0.pos_x` and friends) is stamped with a
/// MIRRORED default, so the plant is skipped and a node write now stands — that
/// is BUG-ji6q, covered by `bound_param_survives_rebuild`. But an import also
/// carries AUTHORED bindings for the nodes that are not scene vocabulary: the
/// `env_mode` enum over `env_select.selector`, `ssao_intensity`, the sun fan-outs
/// onto `envmap.sun_*`, `env_intensity` onto `hdri_gain.gain`. Those defaults are
/// chosen numbers, they still plant, and a caller who sets one on the node
/// instead of the card still loses — silently, which is exactly how
/// `rt_r3_heldout_gltf` measured two raster renders for its whole life. This pins
/// both halves: the write really is lost, and the runtime names it.
#[test]
fn an_imported_defs_authored_binding_still_overwrites_a_node_write_and_reports_it() {
    let mut mat = full_material(0, "Mat", 100);
    mat.own_center = [0.0, 0.0, 0.0];
    let summary = GltfImportSummary {
        materials: vec![mat],
        bbox_min: [-1.0, -1.0, -1.0],
        bbox_max: [1.0, 1.0, 1.0],
        camera_count: 0,
        default_material_vertex_count: 0,
        animations: Vec::new(),
        animation_report_lines: Vec::new(),
        extension_report_lines: Vec::new(),
        lights: Vec::new(),
        cameras: Vec::new(),
        camera_report_lines: Vec::new(),
        texture_dims: Vec::new(),
    };
    let path = std::path::Path::new("/tmp/synthetic_bug1l7f_test.glb");
    let (mut def, _report) = build_import_graph(&summary, path).expect("build import graph");

    // The mistake: switch the environment to HDRI by writing the node param,
    // the way `rt_r3_heldout_gltf` set `rt_enabled` on `render_scene`.
    let node_id = manifold_core::NodeId::new("env_select");
    let baked = 1.0_f32;
    let node = def
        .nodes
        .iter_mut()
        .find(|n| n.node_id == node_id)
        .expect("env_select present in the imported def");
    node.params.insert(
        "selector".to_string(),
        manifold_core::effect_graph_def::SerializedParamValue::Float { value: baked },
    );

    let registry = PrimitiveRegistry::with_builtin();
    let runtime = PresetRuntime::from_def(def, &registry, None).expect("instantiate imported def");

    let inst = runtime
        .graph
        .instance_by_node_id(&node_id)
        .expect("env_select in the live graph");
    let got = runtime
        .graph
        .get_node(inst)
        .and_then(|n| n.params.get("selector").cloned())
        .expect("selector readable post-build");
    assert_ne!(
        got,
        crate::node_graph::parameters::ParamValue::Float(baked),
        "if an AUTHORED default ever stops planting, this test is the place that \
         records the decision — that plant is where a binding's scale/offset fold \
         is applied, so dropping it renders raw def values (BUG-1l7f)",
    );

    let findings: Vec<_> = runtime.shadowed_def_params().collect();
    assert!(
        findings
            .iter()
            .any(|f| f.node_id == "env_select" && f.param == "selector"),
        "the silent revert must be reported by node and param; got {findings:?}",
    );
}
