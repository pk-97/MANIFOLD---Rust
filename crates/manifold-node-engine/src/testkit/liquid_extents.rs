    use crate::water::primitives::gpu_flip_preset::testkit::fused_as_rendered;
use manifold_core::effect_graph_def::*;
    use crate::exec::extent::{AtomExtent, ExtentError, ExtentReport, ExtentRule, EXTENT_RULES, Verdict, check_graph};
    use crate::testkit::substep_nodes::register_substep_test_nodes;
    use crate::{persistence::EffectGraphDefExt, exec::execution_plan::ExecutionPlan, graph::Graph, persistence::PrimitiveRegistry, exec::execution_plan::compile};


pub fn registry() -> PrimitiveRegistry {
        let mut registry = PrimitiveRegistry::with_builtin();
        register_substep_test_nodes(&mut registry);
        registry
    }
/// Test sources hold what the planner gives them; sinks read nothing.
fn harness_node(_: &mut AtomExtent<'_>) -> Result<(), Verdict> {
        Ok(())
    }
/// Fused regions are the freeze compiler's contract (BUG-2efy (fused
/// output capacity probe)); the walk sizes what they read and write.
fn rules(frozen: bool) -> Vec<ExtentRule> {
        let mut rules = EXTENT_RULES.to_vec();
        for type_id in ["test.value_source", "test.value_sink", "test.liquid_sink", "test.mesh_sink"] {
            rules.push(ExtentRule { type_id, check: harness_node });
        }
        if frozen {
            rules.push(ExtentRule { type_id: "node.wgsl_compute", check: harness_node });
        }
        rules
    }
pub fn built(def: &EffectGraphDef) -> (Graph, ExecutionPlan) {
        let graph = def.clone().into_graph(&registry(), &Default::default()).expect("the def builds");
        let plan = compile(&graph).expect("the def compiles");
        (graph, plan)
    }
/// `def` walked by the liquid extent rules, frozen as the app renders it
/// when `frozen`.
pub fn walk(def: &EffectGraphDef, frozen: bool) -> Result<ExtentReport, ExtentError> {
        let view = frozen.then(|| fused_as_rendered(def, &registry())).flatten();
        let (mut graph, plan) = if let Some(view) = view {
            let graph = (*view.def).clone().into_graph(&registry(), &view.mesh_rules).expect("the fused def builds");
            let plan = compile(&graph).expect("the fused def compiles");
            (graph, plan)
        } else {
            built(def)
        };
        check_graph(&mut graph, &plan, &rules(frozen))
    }
