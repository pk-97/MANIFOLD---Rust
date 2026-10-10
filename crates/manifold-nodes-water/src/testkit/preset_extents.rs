//! The liquid-preset extent harness: build a preset once, walk its extent
//! rules at any resolution of its liquid domains.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_node_engine::exec::execution_plan::{compile, ExecutionPlan};
use manifold_node_engine::exec::extent::{check_graph, ExtentError, ExtentReport, ExtentRule, EXTENT_RULES};
use manifold_node_engine::graph::Graph;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::persistence::{EffectGraphDefExt, PrimitiveRegistry};

/// Build a preset at one resolution of its liquid domain and walk it.
pub fn check_preset_extents(def: &EffectGraphDef, resolution: u32) -> Result<ExtentReport, ExtentError> {
    let mut preset = LiquidPreset::build(def)?;
    preset.check(resolution)
}

/// A liquid preset built once, checked at any resolution of its domain.
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
    pub fn build_with_registry(def: &EffectGraphDef, registry: &PrimitiveRegistry) -> Result<Self, ExtentError> {
        let build = |error: String| ExtentError::Build(error);
        let expanded = manifold_node_engine::load::expand::expand_scene_modifiers(def, registry)
            .map_err(|error| build(error.to_string()))?;
        let flat = manifold_core::flatten::flatten_groups(&expanded).map_err(|error| build(error.to_string()))?;
        let graph = flat.into_graph(registry, &Default::default()).map_err(|error| build(format!("{error:?}")))?;
        let plan = compile(&graph).map_err(|error| build(format!("{error:?}")))?;
        let domains: Vec<_> = graph.nodes().filter(|node| is_liquid_domain(node.node.type_id().as_str())).map(|node| node.id).collect();
        if domains.is_empty() {
            return Err(build("no liquid domain".into()));
        }
        for step in plan.steps() {
            if domains.contains(&step.node) && step.inputs.iter().any(|(port, _)| *port == "resolution") {
                return Err(build("a liquid domain's resolution is wired; the walk sets the param".into()));
            }
        }
        Ok(Self { graph, plan, domains })
    }

    /// Every resolution the domains' Resolution control admits.
    pub fn resolutions(&self) -> std::ops::RangeInclusive<u32> {
        let range = |id| {
            let node = self.graph.get_node(id).expect("domain");
            let def = node.node.parameters().iter().find(|p| p.name == "resolution").expect("a Resolution control");
            let (low, high) = def.range.expect("Resolution has a range");
            (low as u32, high as u32)
        };
        let (low, high) = self.domains.iter().map(|&id| range(id)).fold((0, u32::MAX), |(a, b), (c, d)| (a.max(c), b.min(d)));
        low..=high
    }

    pub fn check(&mut self, resolution: u32) -> Result<ExtentReport, ExtentError> {
        for &id in &self.domains {
            self.graph
                .set_param(id, "resolution", ParamValue::Float(resolution as f32))
                .map_err(|error| ExtentError::Build(format!("{error:?}")))?;
        }
        self.check_authored()
    }

    /// The graph as its def and card set it.
    pub fn check_authored(&mut self) -> Result<ExtentReport, ExtentError> {
        check_graph(&mut self.graph, &self.plan, &EXTENT_RULES)
    }

    /// The type ids and Resolution of the domains, as built.
    pub fn domains(&self) -> Vec<(&str, u32)> {
        self.domains
            .iter()
            .map(|&id| {
                let node = self.graph.get_node(id).expect("domain");
                let resolution = node.params.get("resolution").and_then(ParamValue::as_scalar).unwrap_or(0.0);
                (node.node.type_id().as_str(), resolution.round() as u32)
            })
            .collect()
    }
}

pub mod testkit;
