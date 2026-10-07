//! SCENE_OBJECT_AND_PANEL_V2_DESIGN.md's single-hop invariant, enforced
//! registry-wide: `Object` wires never chain — `node.scene_object` is the
//! sole producer (takes no `Object` input); every legal consumer is a
//! renderer boundary node (`render_scene` from P2 on — extend
//! `ALLOWED_OBJECT_CONSUMERS` the day a second one ships).


use manifold_node_engine::persistence::PrimitiveFactory;
    use manifold_node_engine::ports::PortType;

    /// `type_id`s allowed to declare an `Object`-typed INPUT port. Extending
    /// this list is itself the design's named escalation trigger ("any need
    /// for a second Object consumer/producer" — section 8) — don't add to it
    /// without re-reading the design doc.
    const ALLOWED_OBJECT_CONSUMERS: &[&str] = &["node.render_scene"];

    #[test]
    fn object_port_single_hop() {
        for factory in inventory::iter::<PrimitiveFactory> {
            let node = (factory.create)();
            let has_object_output = node.outputs().iter().any(|o| o.ty == PortType::Object);
            let has_object_input = node.inputs().iter().any(|i| i.ty == PortType::Object);

            assert!(
                !has_object_output || factory.type_id == "node.scene_object",
                "{} declares an Object output but is not node.scene_object — \
                 node.scene_object must be the sole Object producer",
                factory.type_id,
            );
            assert!(
                !has_object_input || ALLOWED_OBJECT_CONSUMERS.contains(&factory.type_id),
                "{} declares an Object input but isn't in ALLOWED_OBJECT_CONSUMERS \
                 — a second Object consumer is a design escalation, not a \
                 mechanical addition (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md section 8)",
                factory.type_id,
            );
        }
    }

    #[test]
    fn scene_object_itself_takes_no_object_input() {
        // Restates the invariant at the single known producer, so a
        // regression here fails loudly and specifically instead of only
        // showing up as a generic registry-walk failure.
        let node = crate::node_graph::primitives::SceneObjectNode::new();
        assert!(
            !manifold_node_engine::exec::effect_node::EffectNode::inputs(&node)
                .iter()
                .any(|i| i.ty == PortType::Object),
            "node.scene_object must not take an Object input — Object wires never chain"
        );
    }
