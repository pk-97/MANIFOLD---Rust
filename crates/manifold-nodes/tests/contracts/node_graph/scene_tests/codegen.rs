use manifold_node_engine::exec::effect_node::NodeInstanceId;
use manifold_node_engine::freeze::markers::Marker;

use manifold_node_engine::freeze::codegen::generate_fused;
use manifold_node_engine::freeze::codegen::{FusionRegion, InputSource, RegionNode};


/// Buffer-domain multi-atom fusion: a chain of two per-element instance atoms
/// fuses into one `var<storage>` kernel. The element struct is synthesized,
/// every input and the output bind as storage arrays, the dispatch is a 1D
/// `arrayLength`-guarded loop (the `node.wgsl_compute` buffer convention, with
/// no `dispatch_count` uniform), the first body's element register threads
/// into the second, and the shared `noise_common` include is prepended once
/// (so parse resolves the helper calls; were the include dropped, naga parse
/// would fail here). The buffer analogue of
/// `fused_gather_binds_sampler_and_passes_texture`. End-to-end numerical
/// parity rides the render-parity oracle once the finder emits buffer regions
/// on the live path.
#[test]
fn fused_buffer_region_threads_element_registers() {
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_nodes_scene::node_graph::primitives::instance_rotation_jitter::InstanceRotationJitter as J;
    let id = NodeInstanceId;
    let mk = |i: u32, src: InputSource| RegionNode {
        node_id: id(i),
        fusion_kind: J::FUSION_KIND,
        body: J::WGSL_BODY.unwrap(),
        params: J::PARAMS,
        inputs: vec![src],
        input_access: J::INPUT_ACCESS.to_vec(),
        node_inputs: J::INPUTS,
        node_outputs: J::OUTPUTS,
        node_includes: J::WGSL_INCLUDES,
        derived_uniforms: J::DERIVED_UNIFORMS,
        type_id: J::TYPE_ID.to_string(),
        derived_camera_ext: None,
        output_storage: "rgba16float",
        stencil_fetch: false,
        quantize_f16: false,
    };
    let region = FusionRegion {
        nodes: vec![mk(0, InputSource::External(0)), mk(1, InputSource::Node(id(0)))],
        num_external_inputs: 1,
        outputs: vec![(id(1), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    output_capacity: None,
    };
    let g = generate_fused(&region).expect("buffer region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused buffer kernel parses through naga (validates the body ABI + includes):\n{}",
        g.wgsl
    );
    // Inputs are read-only (forward deps); the output is a FRESH write-only
    // `dst` tagged `// @fused_output` (not aliased). This is what keeps the
    // node ordered after its producers.
    assert!(
        g.wgsl.contains("var<storage, read> src_0"),
        "external input bound read-only:\n{}",
        g.wgsl
    );
    assert!(g.wgsl.contains(&Marker::FusedOutput.emit()), "fresh output tagged @fused_output");
    assert!(
        g.wgsl.contains("var<storage, read_write> dst:"),
        "fresh dst output array declared:\n{}",
        g.wgsl
    );
    assert!(g.wgsl.contains("arrayLength(&src_0)"), "1D dispatch keyed on an input array length");
    assert!(g.wgsl.contains("let e_0 = src_0[idx];"), "external element pre-read once");
    assert!(g.wgsl.contains("let r0 = n0_body"), "first member's element register");
    assert!(g.wgsl.contains("let r1 = n1_body"), "second member threads r0");
    assert!(g.wgsl.contains("dst[idx] = r1;"), "region result written to the fresh output");
}

/// BUG-008: a buffer region with TWO array externals pre-reads BOTH at `[idx]`.
/// The dispatch count must be bounded by the SHORTER external so neither read
/// goes out of bounds when the two inputs have different lengths (the unfused
/// atoms clamp to `min(a, b, …)` for exactly this reason). `LerpInstanceFields`
/// (two required `Array<InstanceTransform>` inputs) is the shipped shape.
#[test]
fn fused_buffer_region_two_array_externals_bounds_count_by_min() {
    use manifold_node_engine::primitive::PrimitiveSpec;
    use manifold_nodes_scene::node_graph::primitives::lerp_instance_fields::LerpInstanceFields as L;
    let id = NodeInstanceId;
    let region = FusionRegion {
        nodes: vec![RegionNode {
            node_id: id(0),
            fusion_kind: L::FUSION_KIND,
            body: L::WGSL_BODY.unwrap(),
            params: L::PARAMS,
            inputs: vec![InputSource::External(0), InputSource::External(1)],
            input_access: L::INPUT_ACCESS.to_vec(),
            node_inputs: L::INPUTS,
            node_outputs: L::OUTPUTS,
            node_includes: L::WGSL_INCLUDES,
            derived_uniforms: L::DERIVED_UNIFORMS,
            type_id: L::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }],
        num_external_inputs: 2,
        outputs: vec![(id(0), "out".to_string())],
        in_place_alias: None,
        sampler_address_mode: "clamp",
        dispatch_count_field: None,
        virtual_chains: Vec::new(),
        sampled_externals: Vec::new(), camera_externals: 0,
    output_capacity: None,
    };
    let g = generate_fused(&region).expect("two-external buffer region fuses");
    assert!(
        naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
        "fused two-external buffer kernel parses:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl.contains("let e_0 = src_0[idx];") && g.wgsl.contains("let e_1 = src_1[idx];"),
        "both array externals are pre-read at [idx]:\n{}",
        g.wgsl
    );
    assert!(
        g.wgsl
            .contains("let count = min(arrayLength(&src_0), arrayLength(&src_1));"),
        "count bounded by the SHORTER external so neither pre-read is OOB (BUG-008):\n{}",
        g.wgsl
    );
}
