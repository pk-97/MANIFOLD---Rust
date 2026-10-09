//! Generic graph ordering for ordered node pairs.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use ahash::{AHashMap, AHashSet};

use crate::exec::effect_node::{AsAny, EffectNode, EffectNodeContext, NodeInstanceId};
use crate::graph::{Graph, WireWalkMode};
use crate::validation::GraphError;

/// Family-owned callbacks for one ordered pair of graph nodes.
pub trait NodePairBehavior: AsAny + Send {
    fn set_enabled(&self, node: &mut dyn EffectNode, enabled: bool);
    fn before_first(
        &self,
        first: &mut dyn EffectNode,
        second: &mut dyn EffectNode,
        second_inputs: Option<&mut EffectNodeContext<'_, '_>>,
    );
    fn after_first(&self, first: &dyn EffectNode, second: &mut dyn EffectNode);
}

/// One ordered pair of graph nodes that must be evaluated as one unit.
pub struct NodePair {
    pub first: NodeInstanceId,
    pub second: NodeInstanceId,
    pub behavior: Box<dyn NodePairBehavior>,
}

/// Final execution-step positions for one ordered node pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodePairSteps {
    pub first_step: usize,
    pub second_step: usize,
    pub pair_index: usize,
}

struct ContractedGroup {
    members: Vec<NodeInstanceId>,
    first_position: usize,
    /// A substep region wires its own members together; a node pair must not.
    internal_wires_allowed: bool,
}

/// The liveness-filtered topological order: every node when the graph has no
/// liveness root, otherwise the nodes reachable from one.
pub(crate) fn active_execution_order(
    graph: &Graph,
    full_order: &[NodeInstanceId],
    has_liveness_root: bool,
) -> Vec<NodeInstanceId> {
    let active = active_nodes(graph, full_order, has_liveness_root);
    full_order
        .iter()
        .copied()
        .filter(|id| active.contains(id))
        .collect()
}

/// Contract each active ordered pair and each substep region block into one
/// vertex, then expand them in place. Pair order is first then second; a
/// region remains a contiguous block with its boundary first.
pub(crate) fn contracted_execution_order(
    graph: &Graph,
    active_order: &[NodeInstanceId],
    blocks: &[Vec<NodeInstanceId>],
) -> Result<(Vec<NodeInstanceId>, Vec<NodePairSteps>), GraphError> {
    let active_order = active_order.to_vec();
    if graph.node_pairs().is_empty() && blocks.is_empty() {
        return Ok((active_order, Vec::new()));
    }
    let active: AHashSet<NodeInstanceId> = active_order.iter().copied().collect();
    let mut block_for = AHashMap::<NodeInstanceId, usize>::default();
    for (block_index, block) in blocks.iter().enumerate() {
        for &node in block {
            if block_for.insert(node, block_index).is_some() {
                return Err(GraphError::CycleDetected {
                    involves: block.clone(),
                });
            }
        }
    }

    let mut pair_for = AHashMap::<NodeInstanceId, usize>::default();
    // Keep the original graph index here. Dead pairs are filtered after this
    // enumeration, so plan metadata can still address the behavior correctly.
    for (pair_index, pair) in graph.node_pairs().iter().enumerate() {
        if !active.contains(&pair.first) || !active.contains(&pair.second) {
            continue;
        }
        for node in [pair.first, pair.second] {
            if pair_for.insert(node, pair_index).is_some() || block_for.contains_key(&node) {
                return Err(GraphError::CycleDetected {
                    involves: vec![pair.first, pair.second],
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
        let group_index = groups.len();
        if let Some(&block_index) = block_for.get(&node) {
            let block = &blocks[block_index];
            for &member in block {
                group_for.insert(member, group_index);
            }
            groups.push(ContractedGroup {
                members: block.clone(),
                first_position: position,
                internal_wires_allowed: true,
            });
        } else if let Some(&pair_index) = pair_for.get(&node) {
            let pair = &graph.node_pairs()[pair_index];
            let first_position = active_order
                .iter()
                .position(|&candidate| candidate == pair.first || candidate == pair.second)
                .unwrap_or(position);
            groups.push(ContractedGroup {
                members: vec![pair.first, pair.second],
                first_position,
                internal_wires_allowed: false,
            });
            group_for.insert(pair.first, group_index);
            group_for.insert(pair.second, group_index);
        } else {
            groups.push(ContractedGroup {
                members: vec![node],
                first_position: position,
                internal_wires_allowed: false,
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
            if groups[from].internal_wires_allowed {
                continue;
            }
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
    let mut node_pairs = graph
        .node_pairs()
        .iter()
        .enumerate()
        .filter_map(|(pair_index, pair)| {
            let first_step = step_by_node.get(&pair.first).copied()?;
            let second_step = step_by_node.get(&pair.second).copied()?;
            Some(NodePairSteps { first_step, second_step, pair_index })
        })
        .collect::<Vec<_>>();
    node_pairs.sort_by_key(|pair| (pair.first_step, pair.second_step));
    Ok((order, node_pairs))
}

fn active_nodes(
    graph: &Graph,
    full_order: &[NodeInstanceId],
    has_liveness_root: bool,
) -> AHashSet<NodeInstanceId> {
    if !has_liveness_root {
        return full_order.iter().copied().collect();
    }

    crate::validation::reachable_from_liveness_roots(graph)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    use crate::exec::effect_node::EffectNodeType;
    use crate::graph::Graph;
    use crate::ports::{NodeInput, NodeOutput, NodePort, PortKind, PortType};

    pub(crate) struct OrderingOnly;

    impl NodePairBehavior for OrderingOnly {
        fn set_enabled(&self, _node: &mut dyn EffectNode, _enabled: bool) {}

        fn before_first(
            &self,
            _first: &mut dyn EffectNode,
            _second: &mut dyn EffectNode,
            _second_inputs: Option<&mut EffectNodeContext<'_, '_>>,
        ) {
        }

        fn after_first(&self, _first: &dyn EffectNode, _second: &mut dyn EffectNode) {}
    }

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
        fn type_id(&self) -> &EffectNodeType { &self.type_id }
        fn inputs(&self) -> &[NodeInput] { &self.inputs }
        fn outputs(&self) -> &[NodeOutput] { &self.outputs }
        fn parameters(&self) -> &[crate::parameters::ParamDef] { &[] }
        fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
            crate::scene::depth_rule::DepthRule::Terminal
        }
        fn evaluate(&mut self, _ctx: &mut EffectNodeContext<'_, '_>) {}
        fn is_liveness_root(&self) -> bool { self.root }
    }

    fn node(graph: &mut Graph, name: &'static str, inputs: &[&'static str], outputs: &[&'static str]) -> NodeInstanceId {
        graph.add_node(Box::new(TestNode::new(name, inputs, outputs)))
    }

    fn root_node(graph: &mut Graph, name: &'static str, inputs: &[&'static str], outputs: &[&'static str]) -> NodeInstanceId {
        let mut node = TestNode::new(name, inputs, outputs);
        node.root = true;
        graph.add_node(Box::new(node))
    }

    fn pair(graph: &mut Graph, first: NodeInstanceId, second: NodeInstanceId) {
        graph
            .add_node_pair(first, second, Box::new(OrderingOnly))
            .unwrap();
    }

    fn execution_order(
        graph: &Graph,
        full: &[NodeInstanceId],
        has_liveness_root: bool,
    ) -> Result<(Vec<NodeInstanceId>, Vec<NodePairSteps>), GraphError> {
        let active = active_execution_order(graph, full, has_liveness_root);
        contracted_execution_order(graph, &active, &[])
    }

    #[test]
    fn shared_ancestors_run_before_fluid_and_rigid_and_consumers_follow_both() {
        let mut graph = Graph::new();
        let ancestor = node(&mut graph, "ancestor", &[], &["out"]);
        let first = node(&mut graph, "first", &["in"], &["out"]);
        let second = node(&mut graph, "second", &["in"], &["out"]);
        let first_consumer = node(&mut graph, "first_consumer", &["in"], &[]);
        let second_consumer = node(&mut graph, "second_consumer", &["in"], &[]);
        graph.connect((ancestor, "out"), (first, "in")).unwrap();
        graph.connect((ancestor, "out"), (second, "in")).unwrap();
        graph.connect((first, "out"), (first_consumer, "in")).unwrap();
        graph.connect((second, "out"), (second_consumer, "in")).unwrap();
        pair(&mut graph, first, second);

        let full = crate::validation::topological_sort(&graph).unwrap();
        let (order, pairs) = execution_order(&graph, &full, false).unwrap();
        let first_step = order.iter().position(|&id| id == first).unwrap();
        let second_step = order.iter().position(|&id| id == second).unwrap();
        assert_eq!(second_step, first_step + 1);
        assert!(order.iter().position(|&id| id == ancestor).unwrap() < first_step);
        assert!(order.iter().position(|&id| id == first_consumer).unwrap() > second_step);
        assert!(order.iter().position(|&id| id == second_consumer).unwrap() > second_step);
        assert_eq!(pairs[0].first_step, first_step);
        assert_eq!(pairs[0].second_step, second_step);
        assert_eq!(pairs[0].pair_index, 0);

        let plan = crate::exec::execution_plan::compile(&graph).unwrap();
        let pair = plan.node_pairs()[0];
        let ancestor_resource = plan
            .steps()
            .iter()
            .find(|step| step.node == ancestor)
            .unwrap()
            .outputs[0]
            .1;
        assert!(plan.steps()[pair.second_step]
            .free_after
            .contains(&ancestor_resource));
        assert!(!plan.steps()[pair.first_step]
            .free_after
            .contains(&ancestor_resource));
        for participant in [pair.first_step, pair.second_step] {
            let resource = plan.steps()[participant].outputs[0].1;
            let free_step = plan
                .steps()
                .iter()
                .position(|step| step.free_after.contains(&resource))
                .unwrap();
            assert!(free_step > pair.second_step);
        }
        let split = plan.truncated(pair.first_step + 1);
        assert_eq!(split.steps().len(), pair.first_step);
        assert!(split.node_pairs().is_empty());
        let complete = plan.truncated(pair.second_step + 1);
        assert_eq!(complete.node_pairs(), &[pair]);
    }

    #[test]
    fn direct_dependency_inside_pair_is_rejected() {
        let mut graph = Graph::new();
        let first = node(&mut graph, "first", &[], &["out"]);
        let second = node(&mut graph, "second", &["in"], &[]);
        graph.connect((first, "out"), (second, "in")).unwrap();
        pair(&mut graph, first, second);
        let full = crate::validation::topological_sort(&graph).unwrap();
        assert!(matches!(execution_order(&graph, &full, false), Err(GraphError::CycleDetected { .. })));
    }

    #[test]
    fn indirect_dependency_created_by_pair_contraction_is_rejected() {
        let mut graph = Graph::new();
        let first = node(&mut graph, "first", &[], &["out"]);
        let second = node(&mut graph, "second", &["in"], &["out"]);
        let middle = node(&mut graph, "middle", &["in"], &["out"]);
        graph.connect((first, "out"), (middle, "in")).unwrap();
        graph.connect((middle, "out"), (second, "in")).unwrap();
        pair(&mut graph, first, second);
        let full = crate::validation::topological_sort(&graph).unwrap();
        assert!(matches!(execution_order(&graph, &full, false), Err(GraphError::CycleDetected { .. })));
    }

    #[test]
    fn independent_pairs_stay_adjacent_and_independent() {
        let mut graph = Graph::new();
        let first_a = node(&mut graph, "first_a", &[], &[]);
        let second_a = node(&mut graph, "second_a", &[], &[]);
        let first_b = node(&mut graph, "first_b", &[], &[]);
        let second_b = node(&mut graph, "second_b", &[], &[]);
        pair(&mut graph, first_a, second_a);
        pair(&mut graph, first_b, second_b);
        let full = crate::validation::topological_sort(&graph).unwrap();
        let (order, pairs) = execution_order(&graph, &full, false).unwrap();
        assert_eq!(pairs.len(), 2);
        for pair in pairs {
            assert_eq!(pair.second_step, pair.first_step + 1);
            assert!((order[pair.first_step] == first_a && order[pair.second_step] == second_a)
                || (order[pair.first_step] == first_b && order[pair.second_step] == second_b));
        }
    }

    #[test]
    fn liveness_closes_live_pair_ancestry_and_drops_dead_pairs() {
        let mut graph = Graph::new();
        let ancestor = node(&mut graph, "ancestor", &[], &["out"]);
        let live_first = root_node(&mut graph, "live_first", &["in"], &[]);
        let live_second = node(&mut graph, "live_second", &["in"], &[]);
        graph.connect((ancestor, "out"), (live_first, "in")).unwrap();
        graph.connect((ancestor, "out"), (live_second, "in")).unwrap();

        let dead_first = node(&mut graph, "dead_first", &[], &[]);
        let dead_second = node(&mut graph, "dead_second", &[], &[]);
        pair(&mut graph, dead_first, dead_second);
        pair(&mut graph, live_first, live_second);

        let full = crate::validation::topological_sort(&graph).unwrap();
        let (order, pairs) = execution_order(&graph, &full, true).unwrap();
        assert!(order.contains(&ancestor));
        assert!(order.contains(&live_first));
        assert!(order.contains(&live_second));
        assert!(!order.contains(&dead_first));
        assert!(!order.contains(&dead_second));
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].pair_index, 1);
    }

    #[test]
    fn removing_coupled_participant_clears_compile_metadata() {
        let mut graph = Graph::new();
        let first = node(&mut graph, "first", &[], &["out"]);
        let second = node(&mut graph, "second", &[], &[]);
        pair(&mut graph, first, second);
        assert_eq!(
            crate::exec::execution_plan::compile(&graph)
                .unwrap()
                .node_pairs()
                .len(),
            1
        );
        graph.remove_node(first).expect("first participant exists");
        assert!(graph.node_pairs().is_empty());
        let plan = crate::exec::execution_plan::compile(&graph).unwrap();
        assert!(plan.node_pairs().is_empty());
    }
}
