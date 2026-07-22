    //! Regression for the effect-side `system.generator_input` surface.
    //! Effects that include a `system.generator_input` node in their
    //! preset get per-frame scalars (time / beat / aspect / output
    //! dims) pushed to it by the chain runner, the same way generators
    //! do. The standard port-shadows-param machinery then propagates
    //! those scalars to inner primitives via wires — no per-effect
    //! Rust code, no hardcoded `apply_ctx_params_at` match list.
    //!
    //! These tests pin two contracts:
    //! 1. **Splice surface**: a preset that includes
    //!    `system.generator_input` causes [`SpliceResult::generator_input_id`]
    //!    to be `Some`, threaded onto [`EffectSlot::generator_input_node`].
    //! 2. **Per-frame push**: [`PresetRuntime::run`] writes the
    //!    [`PresetContext`]'s `time` / `beat` / `aspect` / output dims
    //!    into the generator_input node's params via `set_param`.
    use super::*;
    use crate::node_graph::ParamValue;
    use manifold_core::PresetTypeId;
    use manifold_core::effect_graph_def::EffectGraphDef;

    fn make_default(ty: PresetTypeId) -> PresetInstance {
        manifold_core::preset_definition_registry::create_default(&ty)
    }

    /// A divergent PresetInstance whose graph contains a
    /// `system.generator_input` node. Uses Invert as the host effect
    /// type so we get a known canonical to override; the divergent def
    /// is what actually drives splicing.
    fn invert_with_generator_input() -> PresetInstance {
        let custom_def: EffectGraphDef = serde_json::from_str(
            r#"{
                "version": 1,
                "name": "test",
                "nodes": [
                    { "id": 0, "typeId": "system.source" },
                    { "id": 1, "typeId": "system.generator_input", "handle": "input" },
                    { "id": 2, "typeId": "node.invert", "handle": "invert" },
                    { "id": 3, "typeId": "system.final_output" }
                ],
                "wires": [
                    { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "in" },
                    { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "in" }
                ]
            }"#,
        )
        .expect("test fixture parses");

        let mut fx = make_default(PresetTypeId::INVERT_COLORS);
        // Mark the divergent path live so try_build picks it up. A divergent
        // def is a structural change, so bump the structure version too.
        fx.graph = Some(custom_def);
        fx.graph_version = fx.graph_version.wrapping_add(1);
        fx.graph_structure_version = fx.graph_structure_version.wrapping_add(1);
        fx
    }

    /// Build-time contract: a divergent def with a
    /// `system.generator_input` node populates the EffectSlot's
    /// `generator_input_node` field.
    #[test]
    fn splice_threads_generator_input_id_onto_effect_slot() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("chain builds with a divergent def including system.generator_input");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_some(),
            "EffectSlot.generator_input_node must populate when the def \
             includes a system.generator_input node — without this the \
             chain runner has nowhere to push frame-context scalars and \
             effects can't react to project time/beat."
        );
    }

    /// Build-time symmetry: presets without `system.generator_input`
    /// leave `EffectSlot.generator_input_node` as `None`. Most
    /// shipping effects today fall in this bucket — the field is
    /// opt-in.
    #[test]
    fn splice_leaves_generator_input_node_none_when_absent() {
        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        // Canonical Invert preset has no system.generator_input.
        let fx = make_default(PresetTypeId::INVERT_COLORS);

        let cg = PresetRuntime::try_build(&[fx], &[], &primitives, &device, None, 256, 256, None, None)
            .expect("Invert chain builds without divergent def");

        let slot = cg
            .effect_nodes
            .first()
            .expect("Invert contributes one effect slot");
        assert!(
            slot.generator_input_node.is_none(),
            "EffectSlot.generator_input_node should stay None when the \
             preset doesn't include a system.generator_input — opt-in surface."
        );
    }

    /// Per-frame contract: after `PresetRuntime::run`, the generator_input
    /// node's `time` / `beat` / `aspect` / `output_width` /
    /// `output_height` params reflect the [`PresetContext`].
    /// Exercises the param-write half of the system; the
    /// scalar-wire-propagation half is covered by the
    /// `generator_input_params_drive_scalar_outputs` test in
    /// `boundary_nodes.rs`.
    #[test]
    fn run_pushes_frame_context_into_generator_input_params() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        // A dummy input texture for `run` to install into the source slot.
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );

        let mut native_enc = device.create_encoder("generator-input-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        let ctx = PresetContext {
            time: 1.5,
            beat: 2.25,
            dt: 1.0 / 60.0,
            width: 1920,
            height: 1080,
            output_width: 3840,
            output_height: 2160,
            aspect: 1920.0 / 1080.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let read = |name: &str| -> Option<f32> {
            node.params.get(name).and_then(|v| match v {
                ParamValue::Float(f) => Some(*f),
                _ => None,
            })
        };
        assert_eq!(read("time"), Some(1.5));
        assert_eq!(read("beat"), Some(2.25));
        // aspect derives from ctx.width / ctx.height (the render-resolution
        // dims, not the upscale-target output_* fields).
        assert!((read("aspect").unwrap() - (1920.0 / 1080.0)).abs() < 1e-5);
        assert_eq!(read("output_width"), Some(3840.0));
        assert_eq!(read("output_height"), Some(2160.0));
    }

    /// `trigger_count` used to stay pinned at 0.0 for
    /// effect-chain generator_input nodes ("clip-side concepts that don't
    /// reach the effect chain"). This is the effect-chain half of the P2
    /// gate — the generator half lives in
    /// `generator_renderer::tests` (`effective_trigger_count_sums_clip_and_audio_and_respects_clip_edge_mode`).
    /// Together they prove the SAME effective count (clip edge + audio
    /// fires) reaches both a generator's own graph and an effect chain on
    /// the same layer.
    #[test]
    fn run_feeds_nonzero_trigger_count_into_generator_input_effect_slot() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = invert_with_generator_input();

        let mut cg =
            PresetRuntime::try_build(std::slice::from_ref(&fx), &[], &primitives, &device, None, 256, 256, None, None)
                .expect("chain builds");

        let gi_id = cg
            .effect_nodes
            .first()
            .and_then(|s| s.generator_input_node)
            .expect("splice populated generator_input_node");

        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GpuTextureFormat::Rgba16Float,
            "test-source-input",
        );
        let mut native_enc = device.create_encoder("generator-input-trigger-count-test");
        let mut gpu = GpuEncoder::new(&mut native_enc, &device);

        // A layer whose generator has been triggered 7 times (clip launches
        // + audio fires, already summed by the caller per §8 D1) — the
        // effect chain on that same layer must see the SAME 7, not the old
        // pinned 0.0.
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 256,
            height: 256,
            output_width: 256,
            output_height: 256,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 7,
        };

        cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx);

        let node = cg
            .graph
            .get_node(gi_id)
            .expect("generator_input node id still valid");
        let trigger_count = node.params.get("trigger_count").and_then(|v| match v {
            ParamValue::Float(f) => Some(*f),
            _ => None,
        });
        assert_eq!(
            trigger_count,
            Some(7.0),
            "effect chain's generator_input.trigger_count must reflect the \
             owning layer's effective count (D5), not stay pinned at 0.0"
        );
    }

    /// §8 D6 — Strobe reachability proof: the bundled Strobe preset's
    /// `clip_trigger` card (Trigger Gate → Envelope Decay → Max-combine with
    /// the beat gate) actually flashes when the layer's effective
    /// `trigger_count` jumps, and does NOT when the card is off. This is the
    /// concrete "kick fires Strobe" acceptance demo at the L1 (graph-value)
    /// level — the live app/stem look is still L4-owed (logged in the design
    /// doc), but this proves the wiring is live, not just present in the JSON.
    #[test]
    fn strobe_clip_trigger_card_flashes_on_trigger_count_jump_when_enabled() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();

        let run_and_read_flash_amount = |clip_trigger_on: bool| -> f32 {
            let mut fx = manifold_core::preset_definition_registry::create_default(
                &PresetTypeId::new("Strobe"),
            );
            if let Some(p) = fx.params.get_mut("clip_trigger") {
                p.value = if clip_trigger_on { 1.0 } else { 0.0 };
                p.base = p.value;
            } else {
                panic!("Strobe must ship a clip_trigger card (§8 D6)");
            }

            let mut cg = PresetRuntime::try_build(
                std::slice::from_ref(&fx),
                &[],
                &primitives,
                &device,
                None,
                64,
                64,
                None,
                None,
            )
            .expect("Strobe chain builds");

            let input = crate::render_target::RenderTarget::new(
                &device,
                64,
                64,
                GpuTextureFormat::Rgba16Float,
                "strobe-test-input",
            );
            let mut native_enc = device.create_encoder("strobe-trigger-test");
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);

            let ctx_at = |trigger_count: u32| PresetContext {
                time: 0.0,
                // beat = 0.0 parks node.beat_gate's square wave at 0 (phase
                // 0.0 < duty 0.5) so the Max-combine isolates the trigger
                // path — a bare beat-gate contribution would confound the
                // assertion below.
                beat: 0.0,
                dt: 1.0 / 60.0,
                width: 64,
                height: 64,
                output_width: 64,
                output_height: 64,
                aspect: 1.0,
                owner_key: 0,
                is_clip_level: false,
                frame_count: 0,
                anim_progress: 0.0,
                trigger_count,
            };

            // Watch combine_gate's scalar I/O — `preview_scalar_io` only
            // captures for a NON-texture-outputting node (`node.math`'s `out`
            // is a bare scalar, unlike `flash`'s image output, which the
            // executor deliberately skips scalar capture for — see
            // `execution.rs`'s preview-capture step: image nodes show their
            // texture, not numbers). `.params` was tried first and rejected:
            // it only reflects bound/set values, never what a port-shadowed
            // wire evaluates to (confirmed by inspection — combine_gate's and
            // flash's `.params` stayed at their authoring defaults across both
            // frames below, even though the wires clearly carried real data).
            cg.set_preview_target(&fx.id, Some(&manifold_core::NodeId::new("combine_gate")));

            // Frame 1: baseline at trigger_count 0, settles initial state.
            cg.run(&mut gpu, &input.texture, &[fx.clone()], &[], &ctx_at(0));
            // Frame 2: the layer's effective count jumps (a kick fired).
            cg.run(&mut gpu, &input.texture, &[fx], &[], &ctx_at(5));

            let (_inputs, outputs) = cg.preview_scalar_io();
            outputs
                .iter()
                .find(|(name, _)| name == "out")
                .map(|(_, v)| *v)
                .expect("combine_gate's watched scalar outputs must include `out`")
        };

        let on = run_and_read_flash_amount(true);
        let off = run_and_read_flash_amount(false);

        // node.envelope_decay snaps to 1.0 THEN decays once by this frame's dt
        // in the same evaluate() call, so the observable post-frame value
        // after a fire is exp(-decay_rate * dt) = exp(-12/60) ≈ 0.819, never
        // a full 1.0 — 0.7 comfortably separates "just fired" from "at rest".
        assert!(
            on > 0.7,
            "clip_trigger ON: a trigger_count jump must snap the envelope \
             (and therefore flash.amount, via the Max-combine) toward 1.0 \
             (observably ~0.82 one frame later), got {on}"
        );
        assert!(
            off < 0.1,
            "clip_trigger OFF: the Trigger Gate must absorb the count jump \
             so flash.amount stays at the beat gate's (parked-at-0) value, got {off}"
        );
    }

    /// **The production main-path proof (design §12.3 step 5).** With the freeze
    /// toggle on (default), [`PresetRuntime::try_build`] renders a canonical
    /// ColorGrade card through the FUSED node, not the 7 atoms: the built chain
    /// graph contains one `node.wgsl_compute` and none of the original
    /// `node.exposure` / `node.mix` workers, and it runs one frame producing an
    /// output texture. This is what puts the optimised fused kernel on screen.
    #[test]
    fn colorgrade_chain_renders_via_fused_node() {
        use crate::preset_context::PresetContext;
        use crate::gpu_encoder::GpuEncoder;

        // Honor the kill-switch: when MANIFOLD_FREEZE is off this path is
        // intentionally the unfused one, so the assertion wouldn't hold.
        if !crate::node_graph::freeze::install::freeze_enabled() {
            return;
        }

        let device = crate::test_device();
        let primitives = PrimitiveRegistry::with_builtin();
        let fx = make_default(PresetTypeId::new("ColorGrade"));

        let mut cg = PresetRuntime::try_build(
            std::slice::from_ref(&fx),
            &[],
            &primitives,
            &device,
            None,
            256,
            256,
            None,
            None,
        )
        .expect("ColorGrade chain builds");

        // Main-path proof: the fused kernel replaced the atom chain.
        let type_ids: Vec<&str> =
            cg.graph.nodes().map(|n| n.node.type_id().as_str()).collect();
        assert!(
            type_ids.contains(&"node.wgsl_compute"),
            "fused chain must contain the fused WGSL node; got {type_ids:?}"
        );
        assert!(
            !type_ids.contains(&"node.exposure") && !type_ids.contains(&"node.mix"),
            "fused chain must NOT still contain unfused ColorGrade atoms; got {type_ids:?}"
        );

        // And it renders one frame, producing an output texture (the fused
        // kernel actually dispatched through the production chain).
        let input = crate::render_target::RenderTarget::new(
            &device,
            256,
            256,
            GRAPH_FORMAT,
            "cg-fused-input",
        );
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: 256,
            height: 256,
            output_width: 256,
            output_height: 256,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        let mut native_enc = device.create_encoder("cg-fused-run");
        {
            let mut gpu = GpuEncoder::new(&mut native_enc, &device);
            let out =
                cg.run(&mut gpu, &input.texture, std::slice::from_ref(&fx), &[], &ctx);
            assert!(out.is_some(), "fused ColorGrade chain produced an output texture");
        }
        native_enc.commit_and_wait_completed();
    }
