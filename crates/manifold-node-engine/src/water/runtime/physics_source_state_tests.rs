//! Exercise source installation through the EffectNode contract with card-local IDs.
use super::physics_source_state::PhysicsSourceState;
use super::physics_sources::PhysicsSourceGraph;
use crate::{exec::effect_node::EffectNode, exec::effect_node::EffectNodeContext, exec::effect_node::EffectNodeType, graph::Graph, ports::NodeInput, exec::effect_node::NodeInstanceId, ports::NodeOutput, parameters::ParamDef, parameters::ParamValue};
use manifold_core::{NodeId, PresetTypeId, effects::PresetInstance};
use std::cell::RefCell;

mod assets;

type Identity = Result<[u8; 32], String>;
thread_local! {
    static OBSERVED: RefCell<[Option<Identity>; 2]> = const { RefCell::new([None, None]) };
    static PUBLICATIONS: RefCell<[usize; 2]> = const { RefCell::new([0, 0]) };
}

struct SourceObserver(usize, EffectNodeType);
impl EffectNode for SourceObserver {
    fn depth_rule(&self) -> crate::scene::depth_rule::DepthRule {
        crate::scene::depth_rule::DepthRule::Terminal
    }
    fn type_id(&self) -> &EffectNodeType {
        &self.1
    }
    fn inputs(&self) -> &[NodeInput] {
        &[]
    }
    fn outputs(&self) -> &[NodeOutput] {
        &[]
    }
    fn parameters(&self) -> &[ParamDef] {
        static PARAMETERS: std::sync::LazyLock<[ParamDef; 1]> = std::sync::LazyLock::new(|| {
            [ParamDef {
                name: "selector".into(),
                label: "Selector",
                ty: crate::parameters::ParamType::String,
                default: ParamValue::String(std::sync::Arc::new("alpha".into())),
                range: None,
                enum_values: &[],
            }]
        });
        &*PARAMETERS
    }
    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}
    fn set_physics_source_identity(&mut self, identity: Identity) {
        OBSERVED.with(|observed| observed.borrow_mut()[self.0] = Some(identity));
        PUBLICATIONS.with(|count| count.borrow_mut()[self.0] += 1);
    }
}

#[test]
fn string_observations_are_scoped_retained_and_reinstalled_after_rebuild() {
    OBSERVED.with(|observed| *observed.borrow_mut() = [None, None]);
    PUBLICATIONS.with(|count| *count.borrow_mut() = [0, 0]);
    let mut graph = Graph::new();
    let first = graph.add_node(Box::new(SourceObserver(
        0,
        EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
    )));
    let second = graph.add_node(Box::new(SourceObserver(
        1,
        EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
    )));
    let nodes = [
        (NodeId::new("c0.fluid"), first),
        (NodeId::new("c1.fluid"), second),
    ];
    let strings = || {
        let mut sources = source().unwrap();
        sources[0]
            .string_targets
            .push((NodeId::new("fluid"), "selector".into()));
        Ok(sources)
    };
    let mut first_state = PhysicsSourceState::default();
    let mut second_state = PhysicsSourceState::default();
    first_state.apply_prepared(&mut graph, &nodes, "c0.", strings());
    second_state.apply_prepared(&mut graph, &nodes, "c1.", strings());
    let alpha = observed(0);
    assert_eq!(alpha, observed(1), "runtime prefixes do not enter identity");
    first_state.observe_strings(&mut graph);
    first_state.set_instance(&mut graph, None);
    PUBLICATIONS.with(|count| {
        assert_eq!(
            *count.borrow(),
            [1, 1],
            "unchanged observations do not republish"
        )
    });

    graph
        .set_param(
            first,
            "selector",
            ParamValue::String(std::sync::Arc::new("beta".into())),
        )
        .unwrap();
    first_state.observe_strings(&mut graph);
    let beta = observed(0);
    assert_ne!(alpha, beta);
    assert_eq!(observed(1), alpha, "another card stays unchanged");
    first_state.install(&mut graph, &nodes, "c0.");
    assert_eq!(observed(0), beta);
    PUBLICATIONS.with(|count| {
        assert_eq!(
            *count.borrow(),
            [3, 1],
            "explicit installation republishes after native-node harvest"
        )
    });

    let mut fresh = PhysicsSourceState::default();
    graph
        .set_param(
            first,
            "selector",
            ParamValue::String(std::sync::Arc::new("alpha".into())),
        )
        .unwrap();
    fresh.apply_prepared(&mut graph, &nodes, "c0.", strings());
    fresh.carry_controls_from(&first_state);
    fresh.install(&mut graph, &nodes, "c0.");
    assert_eq!(
        observed(0),
        alpha,
        "rebuild reads its own strings rather than carrying old values"
    );

    let mut missing = strings().unwrap();
    missing[0].string_targets[0].0 = NodeId::new("missing");
    fresh.apply_prepared(&mut graph, &nodes, "c0.", Ok(missing));
    assert!(observed(0).unwrap_err().contains("string source 'missing'"));
    assert_eq!(observed(1), alpha);
    fresh.apply_prepared(&mut graph, &nodes, "c0.", strings());
    assert_eq!(observed(0), alpha);
}

fn source() -> Result<Vec<PhysicsSourceGraph>, String> {
    Ok(vec![PhysicsSourceGraph {
        fluid: NodeId::new("fluid"),
        digest: [7; 32],
        control_ids: Vec::new(),
        string_targets: Vec::new(),
        asset_nodes: Vec::new(),
    }])
}

fn observed(index: usize) -> Identity {
    OBSERVED.with(|observed| {
        observed.borrow()[index]
            .clone()
            .expect("source was installed")
    })
}

#[test]
fn duplicate_local_ids_and_segment_prefixes_install_only_the_owning_card() {
    for prefixed in [false, true] {
        OBSERVED.with(|observed| *observed.borrow_mut() = [None, None]);
        let mut graph = Graph::new();
        let first = graph.add_node(Box::new(SourceObserver(
            0,
            EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
        )));
        let second = graph.add_node(Box::new(SourceObserver(
            1,
            EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
        )));
        let first_id = NodeId::new(if prefixed { "c0.fluid" } else { "fluid" });
        let second_id = NodeId::new(if prefixed { "c1.fluid" } else { "fluid" });
        graph.set_node_id(first, first_id.clone());
        graph.set_node_id(second, second_id.clone());
        let all_nodes = [(first_id.clone(), first), (second_id.clone(), second)];
        let first_nodes = [(first_id, first)];
        let second_nodes = [(second_id, second)];
        let first_map = if prefixed {
            &all_nodes[..]
        } else {
            &first_nodes[..]
        };
        let second_map = if prefixed {
            &all_nodes[..]
        } else {
            &second_nodes[..]
        };
        let first_prefix = if prefixed { "c0." } else { "" };
        let second_prefix = if prefixed { "c1." } else { "" };
        let mut first_state = PhysicsSourceState::default();
        let mut second_state = PhysicsSourceState::default();
        first_state.apply_prepared(&mut graph, first_map, first_prefix, source());
        OBSERVED.with(|observed| assert!(observed.borrow()[1].is_none()));
        second_state.apply_prepared(&mut graph, second_map, second_prefix, source());
        assert_eq!(observed(0), Ok([7; 32]));
        assert_eq!(
            observed(1),
            observed(0),
            "prefix must not enter the source digest"
        );

        let instance = PresetInstance::new(PresetTypeId::new("SourceScopes"));
        first_state.set_instance(&mut graph, Some(&instance));
        second_state.set_instance(&mut graph, Some(&instance));
        let valid_identity = observed(1);
        assert_eq!(observed(0), valid_identity);
        first_state.apply_prepared(
            &mut graph,
            first_map,
            first_prefix,
            Err("invalid first source".into()),
        );
        assert_eq!(observed(0), Err("invalid first source".into()));
        assert_eq!(
            observed(1),
            valid_identity,
            "another card's error must not spread"
        );

        first_state.apply_prepared(&mut graph, first_map, first_prefix, source());
        assert!(observed(0).unwrap_err().contains("current host controls"));
        first_state.set_instance(&mut graph, Some(&instance));
        assert_eq!(observed(0), valid_identity);
        assert_eq!(observed(1), valid_identity);
    }
}
