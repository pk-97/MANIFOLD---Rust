//! The liquid-preset extent harness: build a preset once, walk its extent
//! rules at any cell count of its liquid domains (the domain size over the
//! cell size).

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_node_engine::exec::execution_plan::{ExecutionPlan, compile};
use manifold_node_engine::exec::extent::{
    EXTENT_RULES, ExtentError, ExtentReport, ExtentRule, check_graph,
};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};

/// Build a preset with `resolution` cells across its liquid domain and walk it.
pub fn check_preset_extents(
    def: &EffectGraphDef,
    resolution: u32,
) -> Result<ExtentReport, ExtentError> {
    let mut preset = LiquidPreset::build(def)?;
    preset.check(resolution)
}

/// A liquid preset built once, checked at any cell count of its domain.
pub struct LiquidPreset {
    graph: Graph,
    plan: ExecutionPlan,
    domains: Vec<manifold_node_engine::exec::effect_node::NodeInstanceId>,
}

impl LiquidPreset {
    pub fn build(def: &EffectGraphDef) -> Result<Self, ExtentError> {
        let registry = PrimitiveRegistry::with_builtin();
        Self::build_with_registry(def, &registry)
    }

    /// Build with an explicitly selected primitive registry, for proofs that
    /// register their own probe nodes. Product callers use [`Self::build`].
    pub fn build_with_registry(
        def: &EffectGraphDef,
        registry: &PrimitiveRegistry,
    ) -> Result<Self, ExtentError> {
        let build = |error: String| ExtentError::Build(error);
        // Old shapes migrate while the groups are still groups: `into_graph`
        // migrates too, but a flattened document's handles carry group paths
        // that a group added by a migration could not be flattened with.
        let def = manifold_node_engine::load::graph_loader::migrate_graph_shape(
            std::borrow::Cow::Borrowed(def),
        );
        let expanded = manifold_node_engine::load::expand::expand_scene_modifiers(&def, registry)
            .map_err(|error| build(error.to_string()))?;
        let flat = manifold_core::flatten::flatten_groups(&expanded)
            .map_err(|error| build(error.to_string()))?;
        let graph = flat
            .into_graph(registry, &Default::default())
            .map_err(|error| build(format!("{error:?}")))?;
        let plan = compile(&graph).map_err(|error| build(format!("{error:?}")))?;
        let domains: Vec<_> = graph
            .nodes()
            .filter(|node| is_liquid_domain(node.node.type_id().as_str()))
            .map(|node| node.id)
            .collect();
        if domains.is_empty() {
            return Err(build("no liquid domain".into()));
        }
        for step in plan.steps() {
            if domains.contains(&step.node)
                && step.inputs.iter().any(|(port, _)| *port == "cell_metres")
            {
                return Err(build(
                    "a liquid domain's cell size is wired; the walk sets the param".into(),
                ));
            }
        }
        Ok(Self {
            graph,
            plan,
            domains,
        })
    }

    /// Every cell count across the domain the walk tries: the Dam Break
    /// range, 8 to 512 cells.
    pub fn resolutions(&self) -> std::ops::RangeInclusive<u32> {
        8..=512
    }

    pub fn check(&mut self, resolution: u32) -> Result<ExtentReport, ExtentError> {
        for &id in &self.domains {
            let size = self.domain_size(id);
            self.graph
                .set_param(
                    id,
                    "cell_metres",
                    ParamValue::Float(size / resolution as f32),
                )
                .map_err(|error| ExtentError::Build(format!("{error:?}")))?;
        }
        self.check_authored()
    }

    fn domain_size(&self, id: manifold_node_engine::exec::effect_node::NodeInstanceId) -> f32 {
        let node = self.graph.get_node(id).expect("domain");
        node.params
            .get("domain_size")
            .and_then(ParamValue::as_scalar)
            .expect("a Domain Size param")
    }

    /// The graph as its def and card set it.
    pub fn check_authored(&mut self) -> Result<ExtentReport, ExtentError> {
        check_graph(&mut self.graph, &self.plan, &EXTENT_RULES)
    }

    /// The type ids and cell count across each domain, as built.
    pub fn domains(&self) -> Vec<(&str, u32)> {
        self.domains
            .iter()
            .map(|&id| {
                let node = self.graph.get_node(id).expect("domain");
                let cell = node
                    .params
                    .get("cell_metres")
                    .and_then(ParamValue::as_scalar)
                    .expect("a Cell Size param");
                let cells = self.domain_size(id) / cell;
                (node.node.type_id().as_str(), cells.round() as u32)
            })
            .collect()
    }
}

pub mod testkit;
