use manifold_node_engine::freeze::codegen::{FusionRegion, InputSource, RegionNode};
    use manifold_node_engine::freeze::codegen::generate_fused;
    use manifold_node_engine::exec::effect_node::NodeInstanceId;
    use manifold_node_engine::primitive::PrimitiveSpec;
    use crate::node_graph::primitives::{LerpInstanceFields as L, NeighborSmooth as N};

    fn member<P: PrimitiveSpec>(
        i: u32,
        inputs: Vec<InputSource>,
    ) -> RegionNode<'static> {
        RegionNode {
            node_id: NodeInstanceId(i),
            fusion_kind: P::FUSION_KIND,
            body: P::WGSL_BODY.unwrap(),
            params: P::PARAMS,
            inputs,
            input_access: P::INPUT_ACCESS.to_vec(),
            node_inputs: P::INPUTS,
            node_outputs: P::OUTPUTS,
            node_includes: P::WGSL_INCLUDES,
            derived_uniforms: P::DERIVED_UNIFORMS,
            type_id: P::TYPE_ID.to_string(),
            derived_camera_ext: None,
            output_storage: "rgba16float",
            stencil_fetch: false,
            quantize_f16: false,
        }
    }

    /// BUG-x72p (scene-mirror-blocked-gather-input-fusion): a `BufferGather`
    /// member's input binds as a read-only `src_<slot>` storage array the body
    /// indexes itself — the standalone `buf_<port>` global renamed to
    /// `src_<slot>`, NO coincident pre-read (a pre-read at `[idx]` would run
    /// off the end of an input shorter than the dispatch count), and NO body
    /// arg. The same slot read coincidently by another member (the finder
    /// dedupes one producer port into one external) still gets its `e_<slot>`
    /// pre-read + register arg — the two read shapes share one binding.
    #[test]
    fn fused_buffer_gather_binds_array_global_without_preread() {
        let region = FusionRegion {
            nodes: vec![
                member::<N>(0, vec![InputSource::External(0)]),
                member::<L>(1, vec![InputSource::External(0), InputSource::Node(NodeInstanceId(0))]),
            ],
            num_external_inputs: 1,
            outputs: vec![(NodeInstanceId(1), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        let g = generate_fused(&region).expect("gathered buffer region fuses");
        assert!(
            naga::front::wgsl::parse_str(&g.wgsl).is_ok(),
            "fused gathered buffer kernel parses through naga:\n{}",
            g.wgsl
        );
        assert!(
            g.wgsl.contains("var<storage, read> src_0"),
            "the gathered wire binds as a read-only storage array:\n{}",
            g.wgsl
        );
        assert!(
            g.wgsl.contains("let e_0 = src_0[idx];"),
            "the coincident consumer's pre-read is still emitted (same slot):\n{}",
            g.wgsl
        );
        assert!(!g.wgsl.contains("buf_in"), "the standalone global name is renamed away:\n{}", g.wgsl);
        assert!(
            g.wgsl.contains("src_0[left_idx]"),
            "the gather body indexes the bound slot global directly:\n{}",
            g.wgsl
        );
        let smooth_call = g
            .wgsl
            .lines()
            .find(|l| l.contains("let r0 = n0_body("))
            .expect("smooth body call");
        assert_eq!(
            smooth_call.trim(),
            "let r0 = n0_body(idx, count, params.n0_grid_size, params.n0_center_weight);",
            "a BufferGather input takes NO element arg — the body reads the global"
        );
        let blend_call = g
            .wgsl
            .lines()
            .find(|l| l.contains("let r1 = n1_body("))
            .expect("blend body call");
        assert!(
            blend_call.contains("e_0, r0"),
            "the coincident consumer threads the pre-read register + smooth's register:\n{}",
            blend_call
        );
        assert!(
            g.wgsl.contains("let count = arrayLength(&src_0);"),
            "single-array-external count anchor is byte-identical to the all-coincident shape"
        );
        assert!(g.wgsl.contains("dst[idx] = r1;"), "region result written to the fresh output");
    }

    /// A `BufferGather` input resolved to anything but an external (a region
    /// register, an unwired port) is a finder bug — the codegen fails closed
    /// instead of silently threading a register.
    #[test]
    fn fused_buffer_gather_rejects_non_external_source() {
        let region = FusionRegion {
            nodes: vec![
                member::<L>(0, vec![InputSource::External(0), InputSource::External(1)]),
                member::<N>(1, vec![InputSource::Node(NodeInstanceId(0))]),
            ],
            num_external_inputs: 2,
            outputs: vec![(NodeInstanceId(1), "out".to_string())],
            in_place_alias: None,
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        assert!(
            generate_fused(&region).is_err(),
            "a gather reading a region register can't be expressed — refuse the region"
        );
    }

    /// A gathered external can never be an in-place alias: it would be read at
    /// body-computed neighbour indices while other invocations write it in the
    /// same dispatch — a cross-thread race the unfused multi-dispatch chain
    /// never has.
    #[test]
    fn fused_buffer_gather_rejects_gathered_inplace_alias() {
        let region = FusionRegion {
            nodes: vec![member::<N>(0, vec![InputSource::External(0)])],
            num_external_inputs: 1,
            outputs: vec![(NodeInstanceId(0), "out".to_string())],
            in_place_alias: Some(0),
            sampler_address_mode: "clamp",
            dispatch_count_field: None,
            virtual_chains: Vec::new(),
            sampled_externals: Vec::new(),
            camera_externals: 0,
            output_capacity: None,
        };
        assert!(
            generate_fused(&region).is_err(),
            "a gathered in-place alias is a read-write race — refuse the region"
        );
    }
