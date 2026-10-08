//! Canvas-seeded capacity ancestry observation.
impl crate::graph::Graph {
    #[doc(hidden)]
    pub fn test_canvas_capacity_lineage(&self, plan: &crate::exec::execution_plan::ExecutionPlan) -> Vec<bool> {
        super::capacity_lineage(self, plan, |node, port| node.node.canvas_sized_array_outputs().contains(&port))
    }
}
