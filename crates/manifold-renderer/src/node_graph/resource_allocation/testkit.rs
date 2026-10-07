//! Canvas-seeded capacity ancestry observation.
impl crate::node_graph::Graph {
    #[doc(hidden)]
    pub(crate) fn test_canvas_capacity_lineage(&self, plan: &crate::node_graph::ExecutionPlan) -> Vec<bool> {
        super::capacity_lineage(self, plan, |node, port| node.node.canvas_sized_array_outputs().contains(&port))
    }
}
