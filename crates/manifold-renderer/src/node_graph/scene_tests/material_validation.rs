use std::borrow::Cow;
use crate::node_graph::{EffectNodeType, EffectNodeContext, ParamDef, Graph, GraphError, NodeInstanceId, validate};
use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};
    use crate::node_graph::FINAL_OUTPUT_TYPE_ID;
    use crate::node_graph::effect_node::ConditionalRequirement;
    use crate::node_graph::material::MaterialKind;

    /// Stand-in for a 3D mesh renderer that requires `light` whenever
    /// the wired material's kind is `Cel`. Mirrors what
    /// `render_3d_mesh` will declare after the M4 tranche.
    struct CelRequiresLightRenderer {
        type_id: EffectNodeType,
    }

    impl CelRequiresLightRenderer {
        fn new() -> Self {
            Self {
                type_id: EffectNodeType::new("test.renderer_cel_needs_light"),
            }
        }
    }

    impl crate::node_graph::EffectNode for CelRequiresLightRenderer {
    fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
        crate::node_graph::depth_rule::DepthRule::Terminal
    }
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }
        fn inputs(&self) -> &[NodeInput] {
            const IN: &[NodeInput] = &[
                NodePort {
                    name: Cow::Borrowed("material"),
                    ty: PortType::Material,
                    kind: PortKind::Input,
                    required: true,
                },
                NodePort {
                    name: Cow::Borrowed("light"),
                    ty: PortType::Light,
                    kind: PortKind::Input,
                    required: false,
                },
            ];
            IN
        }
        fn outputs(&self) -> &[NodeOutput] {
            const OUT: &[NodeOutput] = &[NodePort {
                name: Cow::Borrowed("color"),
                ty: PortType::Texture2D,
                kind: PortKind::Output,
                required: false,
            }];
            OUT
        }
        fn parameters(&self) -> &[ParamDef] {
            &[]
        }
        fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
        fn is_liveness_root(&self) -> bool {
            self.type_id.as_str() == FINAL_OUTPUT_TYPE_ID
        }
        fn conditional_requirements(&self) -> &'static [ConditionalRequirement] {
            const RULES: &[ConditionalRequirement] = &[ConditionalRequirement {
                on_material_kind: MaterialKind::Cel,
                required_inputs: &["light"],
            }];
            RULES
        }
    }

    /// Convenience: build a minimal "renderer → final output" backbone
    /// the validator's liveness pruner respects.
    fn renderer_to_final(
        g: &mut Graph,
        renderer_id: NodeInstanceId,
    ) -> NodeInstanceId {
        let fin = g.add_node(Box::new(crate::node_graph::FinalOutput::new()));
        g.connect((renderer_id, "color"), (fin, "in")).unwrap();
        fin
    }

    #[test]
    fn conditional_requirement_unmet_when_cel_material_lacks_light() {
        use crate::node_graph::primitives::CelMaterial;

        let mut g = Graph::new();
        let mat = g.add_node(Box::new(CelMaterial::new()));
        let renderer = g.add_node(Box::new(CelRequiresLightRenderer::new()));
        g.connect((mat, "out"), (renderer, "material")).unwrap();
        let _ = renderer_to_final(&mut g, renderer);

        match validate(&g) {
            Err(GraphError::ConditionalRequirementUnmet {
                node,
                material_kind,
                missing_input,
            }) => {
                assert_eq!(node, renderer);
                assert_eq!(material_kind, MaterialKind::Cel);
                assert_eq!(missing_input, "light");
            }
            other => panic!("expected ConditionalRequirementUnmet, got {other:?}"),
        }
    }

    #[test]
    fn conditional_requirement_satisfied_with_light_wired() {
        use crate::node_graph::primitives::{CelMaterial, LightNode};

        let mut g = Graph::new();
        let mat = g.add_node(Box::new(CelMaterial::new()));
        let light = g.add_node(Box::new(LightNode::new()));
        let renderer = g.add_node(Box::new(CelRequiresLightRenderer::new()));
        g.connect((mat, "out"), (renderer, "material")).unwrap();
        g.connect((light, "out"), (renderer, "light")).unwrap();
        let _ = renderer_to_final(&mut g, renderer);

        assert!(validate(&g).is_ok());
    }

    #[test]
    fn unlit_material_skips_cel_rule_so_no_light_required() {
        // A renderer that only requires `light` on Cel should be
        // happy with an Unlit material and no light wired — the rule
        // doesn't fire for Unlit.
        use crate::node_graph::primitives::UnlitMaterial;

        let mut g = Graph::new();
        let mat = g.add_node(Box::new(UnlitMaterial::new()));
        let renderer = g.add_node(Box::new(CelRequiresLightRenderer::new()));
        g.connect((mat, "out"), (renderer, "material")).unwrap();
        let _ = renderer_to_final(&mut g, renderer);

        assert!(validate(&g).is_ok());
    }
