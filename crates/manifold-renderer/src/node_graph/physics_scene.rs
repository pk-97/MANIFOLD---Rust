//! Runtime graph bindings and plan-order preparation for coupled physics scenes.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use ahash::{AHashMap, AHashSet};

use crate::node_graph::effect_node::NodeInstanceId;
use crate::node_graph::graph::{Graph, WireWalkMode};
use crate::node_graph::physics::RigidImpulseTargets;
use crate::node_graph::validation::GraphError;

/// One resolved pair of graph nodes that must be evaluated as one scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CoupledScene {
    pub(crate) fluid: NodeInstanceId,
    pub(crate) rigid: NodeInstanceId,
    pub(crate) colliders: RigidImpulseTargets,
}

/// Final execution-step positions for one coupled scene.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CoupledSceneSteps {
    pub(crate) fluid_step: usize,
    pub(crate) rigid_step: usize,
    pub(crate) colliders: RigidImpulseTargets,
}

struct ContractedGroup {
    members: Vec<NodeInstanceId>,
    first_position: usize,
}

/// Filter liveness, contract each active pair, and expand pair groups as
/// adjacent fluid-then-rigid nodes. The input order is the ordinary graph
/// topological order, so the result preserves its stable relative ordering
/// wherever contraction permits.
pub(crate) fn coupled_execution_order(
    graph: &Graph,
    full_order: &[NodeInstanceId],
    has_liveness_root: bool,
) -> Result<(Vec<NodeInstanceId>, Vec<CoupledSceneSteps>), GraphError> {
    let active = active_nodes(graph, full_order, has_liveness_root);
    let active_order: Vec<_> = full_order
        .iter()
        .copied()
        .filter(|id| active.contains(id))
        .collect();
    if graph.coupled_scenes().is_empty() {
        return Ok((active_order, Vec::new()));
    }

    let mut pair_for = AHashMap::<NodeInstanceId, usize>::default();
    for (pair_index, pair) in graph.coupled_scenes().iter().enumerate() {
        if !active.contains(&pair.fluid) || !active.contains(&pair.rigid) {
            continue;
        }
        for node in [pair.fluid, pair.rigid] {
            if pair_for.insert(node, pair_index).is_some() {
                return Err(GraphError::CycleDetected {
                    involves: vec![pair.fluid, pair.rigid],
                });
            }
        }
    }

    let mut groups = Vec::<ContractedGroup>::new();
    let mut group_for = AHashMap::<NodeInstanceId, usize>::default();
    for (position, &node) in active_order.iter().enumerate() {
        if group_for.contains_key(&node) {
            continue;
        }
        if let Some(&pair_index) = pair_for.get(&node) {
            let pair = graph.coupled_scenes()[pair_index];
            let first_position = active_order
                .iter()
                .position(|&candidate| candidate == pair.fluid || candidate == pair.rigid)
                .unwrap_or(position);
            let group_index = groups.len();
            groups.push(ContractedGroup {
                members: vec![pair.fluid, pair.rigid],
                first_position,
            });
            group_for.insert(pair.fluid, group_index);
            group_for.insert(pair.rigid, group_index);
        } else {
            let group_index = groups.len();
            groups.push(ContractedGroup {
                members: vec![node],
                first_position: position,
            });
            group_for.insert(node, group_index);
        }
    }

    let mut incoming = vec![0usize; groups.len()];
    let mut outgoing = vec![Vec::<usize>::new(); groups.len()];
    let mut edges = AHashSet::<(usize, usize)>::default();
    for wire in graph.walk_wires(WireWalkMode::ForwardOnly) {
        if !active.contains(&wire.from.0) || !active.contains(&wire.to.0) {
            continue;
        }
        let from = group_for[&wire.from.0];
        let to = group_for[&wire.to.0];
        if from == to {
            return Err(GraphError::CycleDetected {
                involves: groups[from].members.clone(),
            });
        }
        if edges.insert((from, to)) {
            outgoing[from].push(to);
            incoming[to] += 1;
        }
    }
    for successors in &mut outgoing {
        successors.sort_by_key(|&group| (groups[group].first_position, group));
    }

    let mut ready = BinaryHeap::new();
    for (group, &degree) in incoming.iter().enumerate() {
        if degree == 0 {
            ready.push(Reverse((groups[group].first_position, group)));
        }
    }
    let mut group_order = Vec::with_capacity(groups.len());
    while let Some(Reverse((_, group))) = ready.pop() {
        group_order.push(group);
        for &successor in &outgoing[group] {
            incoming[successor] -= 1;
            if incoming[successor] == 0 {
                ready.push(Reverse((groups[successor].first_position, successor)));
            }
        }
    }
    if group_order.len() != groups.len() {
        let involves = groups
            .iter()
            .enumerate()
            .filter(|(index, _)| !group_order.contains(index))
            .flat_map(|(_, group)| group.members.iter().copied())
            .collect();
        return Err(GraphError::CycleDetected { involves });
    }

    let mut order = Vec::with_capacity(active_order.len());
    for group in group_order {
        order.extend(groups[group].members.iter().copied());
    }

    let step_by_node: AHashMap<_, _> = order
        .iter()
        .copied()
        .enumerate()
        .map(|(step, node)| (node, step))
        .collect();
    let mut coupled_scenes = graph
        .coupled_scenes()
        .iter()
        .filter_map(|pair| {
            let fluid_step = step_by_node.get(&pair.fluid).copied()?;
            let rigid_step = step_by_node.get(&pair.rigid).copied()?;
            Some(CoupledSceneSteps {
                fluid_step,
                rigid_step,
                colliders: pair.colliders,
            })
        })
        .collect::<Vec<_>>();
    coupled_scenes.sort_by_key(|pair| (pair.fluid_step, pair.rigid_step));
    Ok((order, coupled_scenes))
}

fn active_nodes(
    graph: &Graph,
    full_order: &[NodeInstanceId],
    has_liveness_root: bool,
) -> AHashSet<NodeInstanceId> {
    if !has_liveness_root {
        return full_order.iter().copied().collect();
    }

    crate::node_graph::validation::reachable_from_liveness_roots(graph)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::node_graph::effect_node::{EffectNode, EffectNodeContext, EffectNodeType};
    use crate::node_graph::graph::Graph;
    use crate::node_graph::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

    struct TestNode {
        type_id: EffectNodeType,
        inputs: Vec<NodeInput>,
        outputs: Vec<NodeOutput>,
        root: bool,
    }

    impl TestNode {
        fn new(name: &'static str, inputs: &[&'static str], outputs: &[&'static str]) -> Self {
            Self {
                type_id: EffectNodeType::new(name),
                inputs: inputs
                    .iter()
                    .map(|&name| NodePort {
                        name: name.into(),
                        ty: PortType::Texture2D,
                        kind: PortKind::Input,
                        required: true,
                    })
                    .collect(),
                outputs: outputs
                    .iter()
                    .map(|&name| NodePort {
                        name: name.into(),
                        ty: PortType::Texture2D,
                        kind: PortKind::Output,
                        required: false,
                    })
                    .collect(),
                root: false,
            }
        }
    }

    impl EffectNode for TestNode {
        fn type_id(&self) -> &EffectNodeType {
            &self.type_id
        }

        fn inputs(&self) -> &[NodeInput] {
            &self.inputs
        }

        fn outputs(&self) -> &[NodeOutput] {
            &self.outputs
        }

        fn parameters(&self) -> &[crate::node_graph::parameters::ParamDef] {
            &[]
        }

        fn depth_rule(&self) -> crate::node_graph::depth_rule::DepthRule {
            crate::node_graph::depth_rule::DepthRule::Terminal
        }

        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}

        fn is_liveness_root(&self) -> bool {
            self.root
        }
    }

    fn node(
        graph: &mut Graph,
        name: &'static str,
        inputs: &[&'static str],
        outputs: &[&'static str],
    ) -> NodeInstanceId {
        graph.add_node(Box::new(TestNode::new(name, inputs, outputs)))
    }

    fn root_node(
        graph: &mut Graph,
        name: &'static str,
        inputs: &[&'static str],
        outputs: &[&'static str],
    ) -> NodeInstanceId {
        let mut node = TestNode::new(name, inputs, outputs);
        node.root = true;
        graph.add_node(Box::new(node))
    }

    fn pair(graph: &mut Graph, fluid: NodeInstanceId, rigid: NodeInstanceId) {
        graph
            .add_coupled_scene(
                fluid,
                rigid,
                RigidImpulseTargets {
                    bodies: 1,
                    copies: false,
                },
            )
            .unwrap();
    }

    #[test]
    fn shared_ancestors_run_before_fluid_and_rigid_and_consumers_follow_both() {
        let mut graph = Graph::new();
        let ancestor = node(&mut graph, "ancestor", &[], &["out"]);
        let fluid = node(&mut graph, "fluid", &["in"], &["out"]);
        let rigid = node(&mut graph, "rigid", &["in"], &["out"]);
        let fluid_consumer = node(&mut graph, "fluid_consumer", &["in"], &[]);
        let rigid_consumer = node(&mut graph, "rigid_consumer", &["in"], &[]);
        graph.connect((ancestor, "out"), (fluid, "in")).unwrap();
        graph.connect((ancestor, "out"), (rigid, "in")).unwrap();
        graph
            .connect((fluid, "out"), (fluid_consumer, "in"))
            .unwrap();
        graph
            .connect((rigid, "out"), (rigid_consumer, "in"))
            .unwrap();
        pair(&mut graph, fluid, rigid);

        let full = crate::node_graph::validation::topological_sort(&graph).unwrap();
        let (order, scenes) = coupled_execution_order(&graph, &full, false).unwrap();
        let fluid_step = order.iter().position(|&id| id == fluid).unwrap();
        let rigid_step = order.iter().position(|&id| id == rigid).unwrap();
        assert_eq!(rigid_step, fluid_step + 1);
        assert!(order.iter().position(|&id| id == ancestor).unwrap() < fluid_step);
        assert!(order.iter().position(|&id| id == fluid_consumer).unwrap() > rigid_step);
        assert!(order.iter().position(|&id| id == rigid_consumer).unwrap() > rigid_step);
        assert_eq!(scenes[0].fluid_step, fluid_step);
        assert_eq!(scenes[0].rigid_step, rigid_step);

        let plan = crate::node_graph::execution_plan::compile(&graph).unwrap();
        let pair = plan.coupled_scenes()[0];
        let ancestor_resource = plan
            .steps()
            .iter()
            .find(|step| step.node == ancestor)
            .unwrap()
            .outputs[0]
            .1;
        assert!(
            plan.steps()[pair.rigid_step]
                .free_after
                .contains(&ancestor_resource)
        );
        assert!(
            !plan.steps()[pair.fluid_step]
                .free_after
                .contains(&ancestor_resource)
        );
        for participant in [pair.fluid_step, pair.rigid_step] {
            let resource = plan.steps()[participant].outputs[0].1;
            let free_step = plan
                .steps()
                .iter()
                .position(|step| step.free_after.contains(&resource))
                .unwrap();
            assert!(free_step > pair.rigid_step);
        }
        let split = plan.truncated(pair.fluid_step + 1);
        assert_eq!(split.steps().len(), pair.fluid_step);
        assert!(split.coupled_scenes().is_empty());
        let complete = plan.truncated(pair.rigid_step + 1);
        assert_eq!(complete.coupled_scenes(), &[pair]);
    }

    #[test]
    fn direct_dependency_inside_pair_is_rejected() {
        let mut graph = Graph::new();
        let fluid = node(&mut graph, "fluid", &[], &["out"]);
        let rigid = node(&mut graph, "rigid", &["in"], &[]);
        graph.connect((fluid, "out"), (rigid, "in")).unwrap();
        pair(&mut graph, fluid, rigid);
        let full = crate::node_graph::validation::topological_sort(&graph).unwrap();
        assert!(matches!(
            coupled_execution_order(&graph, &full, false),
            Err(GraphError::CycleDetected { .. })
        ));
    }

    #[test]
    fn indirect_dependency_created_by_pair_contraction_is_rejected() {
        let mut graph = Graph::new();
        let fluid = node(&mut graph, "fluid", &[], &["out"]);
        let rigid = node(&mut graph, "rigid", &["in"], &["out"]);
        let middle = node(&mut graph, "middle", &["in"], &["out"]);
        graph.connect((fluid, "out"), (middle, "in")).unwrap();
        graph.connect((middle, "out"), (rigid, "in")).unwrap();
        pair(&mut graph, fluid, rigid);
        let full = crate::node_graph::validation::topological_sort(&graph).unwrap();
        assert!(matches!(
            coupled_execution_order(&graph, &full, false),
            Err(GraphError::CycleDetected { .. })
        ));
    }

    #[test]
    fn independent_pairs_stay_adjacent_and_independent() {
        let mut graph = Graph::new();
        let fluid_a = node(&mut graph, "fluid_a", &[], &[]);
        let rigid_a = node(&mut graph, "rigid_a", &[], &[]);
        let fluid_b = node(&mut graph, "fluid_b", &[], &[]);
        let rigid_b = node(&mut graph, "rigid_b", &[], &[]);
        pair(&mut graph, fluid_a, rigid_a);
        pair(&mut graph, fluid_b, rigid_b);
        let full = crate::node_graph::validation::topological_sort(&graph).unwrap();
        let (order, scenes) = coupled_execution_order(&graph, &full, false).unwrap();
        assert_eq!(scenes.len(), 2);
        for scene in scenes {
            assert_eq!(scene.rigid_step, scene.fluid_step + 1);
            assert!(
                (order[scene.fluid_step] == fluid_a && order[scene.rigid_step] == rigid_a)
                    || (order[scene.fluid_step] == fluid_b && order[scene.rigid_step] == rigid_b)
            );
        }
    }

    #[test]
    fn liveness_closes_live_pair_ancestry_and_drops_dead_pairs() {
        let mut graph = Graph::new();
        let ancestor = node(&mut graph, "ancestor", &[], &["out"]);
        let fluid = root_node(&mut graph, "fluid", &["in"], &[]);
        let rigid = node(&mut graph, "rigid", &["in"], &[]);
        graph.connect((ancestor, "out"), (fluid, "in")).unwrap();
        graph.connect((ancestor, "out"), (rigid, "in")).unwrap();
        pair(&mut graph, fluid, rigid);

        let dead_fluid = node(&mut graph, "dead_fluid", &[], &[]);
        let dead_rigid = node(&mut graph, "dead_rigid", &[], &[]);
        pair(&mut graph, dead_fluid, dead_rigid);

        let full = crate::node_graph::validation::topological_sort(&graph).unwrap();
        let (order, scenes) = coupled_execution_order(&graph, &full, true).unwrap();
        assert!(order.contains(&ancestor));
        assert!(order.contains(&fluid));
        assert!(order.contains(&rigid));
        assert!(!order.contains(&dead_fluid));
        assert!(!order.contains(&dead_rigid));
        assert_eq!(scenes.len(), 1);
    }

    fn built_in_coupled_splice_fixture() -> manifold_core::effect_graph_def::EffectGraphDef {
        serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                {"id": 0, "nodeId": "source", "typeId": "system.source"},
                {"id": 1, "nodeId": "scene", "typeId": "node.render_scene",
                    "params": {"objects": {"type": "Int", "value": 2}}},
                {"id": 2, "nodeId": "final", "typeId": "system.final_output"},
                {"id": 3, "nodeId": "world", "typeId": "node.physics_world"},
                {"id": 4, "nodeId": "world_body", "typeId": "node.rigid_body"},
                {"id": 5, "nodeId": "fluid", "typeId": "node.fluid_surface"},
                {"id": 6, "nodeId": "body_object", "typeId": "node.scene_object"},
                {"id": 7, "nodeId": "fluid_object", "typeId": "node.scene_object"}
            ],
            "wires": [
                {"fromNode": 4, "fromPort": "body", "toNode": 3, "toPort": "body_0"},
                {"fromNode": 3, "fromPort": "pose_0", "toNode": 6, "toPort": "transform"},
                {"fromNode": 6, "fromPort": "object", "toNode": 1, "toPort": "object_0"},
                {"fromNode": 5, "fromPort": "vertices", "toNode": 7, "toPort": "vertices"},
                {"fromNode": 7, "fromPort": "object", "toNode": 1, "toPort": "object_1"},
                {"fromNode": 1, "fromPort": "color", "toNode": 2, "toPort": "in"}
            ]
        }))
        .expect("built-in coupled splice fixture parses")
    }

    #[test]
    fn repeated_coupled_splices_use_each_local_id_map() {
        use crate::node_graph::boundary_nodes::Source;
        use crate::node_graph::graph_loader::{BoundaryHandling, HandleScope, instantiate_def};
        use crate::node_graph::mesh_change::PreparedMeshRules;
        use crate::node_graph::persistence::PrimitiveRegistry;

        let def = built_in_coupled_splice_fixture();
        let registry = PrimitiveRegistry::with_builtin();
        let mut graph = Graph::new();
        let host_a = graph.add_node(Box::new(Source::new()));
        let host_b = graph.add_node(Box::new(Source::new()));
        let first = instantiate_def(
            &mut graph,
            &def,
            &registry,
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_a, "out"),
            },
            &PreparedMeshRules::default(),
        )
        .expect("first built-in splice instantiates");
        let second = instantiate_def(
            &mut graph,
            &def,
            &registry,
            HandleScope::PerSplice,
            BoundaryHandling::Splice {
                source_endpoint: (host_b, "out"),
            },
            &PreparedMeshRules::default(),
        )
        .expect("second built-in splice instantiates");

        let first_fluid = first.id_map[&5];
        let first_rigid = first.id_map[&3];
        let second_fluid = second.id_map[&5];
        let second_rigid = second.id_map[&3];
        assert_ne!(first_fluid, second_fluid);
        assert_ne!(first_rigid, second_rigid);
        assert_eq!(
            graph
                .coupled_scenes()
                .iter()
                .map(|pair| (pair.fluid, pair.rigid))
                .collect::<Vec<_>>(),
            vec![(first_fluid, first_rigid), (second_fluid, second_rigid)]
        );
    }

    #[test]
    fn removing_coupled_participant_clears_compile_metadata() {
        let mut graph = Graph::new();
        let fluid = node(&mut graph, "fluid", &[], &["out"]);
        let rigid = node(&mut graph, "rigid", &[], &[]);
        pair(&mut graph, fluid, rigid);
        assert_eq!(
            crate::node_graph::execution_plan::compile(&graph)
                .unwrap()
                .coupled_scenes()
                .len(),
            1
        );

        graph.remove_node(fluid).expect("fluid participant exists");
        assert!(graph.coupled_scenes().is_empty());
        let plan = crate::node_graph::execution_plan::compile(&graph).unwrap();
        assert!(plan.coupled_scenes().is_empty());
    }
}
