//! Construction-time hooks for family-owned graph installation.

use ahash::AHashMap;
use manifold_core::effect_graph_def::EffectGraphDef;

use crate::exec::effect_node::NodeInstanceId;
use crate::graph::Graph;
use crate::load::graph_loader::GraphBuildError;
use crate::persistence::PrimitiveRegistry;

pub type GraphInstaller = fn(
    &EffectGraphDef,
    &PrimitiveRegistry,
    &AHashMap<u32, NodeInstanceId>,
    &mut Graph,
) -> Result<(), GraphBuildError>;

/// A named callback that installs family-owned graph metadata after wires are
/// connected and before prepared rules and budgets are installed.
pub struct GraphInstantiationHook {
    pub name: &'static str,
    pub apply: GraphInstaller,
}

inventory::collect!(GraphInstantiationHook);

/// Run all registered graph-installation hooks in stable name order.
pub(crate) fn run(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    id_map: &AHashMap<u32, NodeInstanceId>,
    graph: &mut Graph,
) -> Result<(), GraphBuildError> {
    let mut hooks: Vec<_> = inventory::iter::<GraphInstantiationHook>
        .into_iter()
        .collect();
    hooks.sort_unstable_by_key(|hook| hook.name);
    for pair in hooks.windows(2) {
        assert_ne!(
            pair[0].name, pair[1].name,
            "duplicate graph instantiation hook name"
        );
    }
    for hook in hooks {
        (hook.apply)(def, registry, id_map, graph)?;
    }
    Ok(())
}
