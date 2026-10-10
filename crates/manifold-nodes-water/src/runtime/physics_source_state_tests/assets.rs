//! Exercise per-card provenance for external source assets.
use super::*;

use manifold_node_engine::scene::source_asset::SourceAssetIdentity;

struct AssetProbe(EffectNodeType);

impl EffectNode for AssetProbe {
    fn depth_rule(&self) -> manifold_node_engine::scene::depth_rule::DepthRule {
        manifold_node_engine::scene::depth_rule::DepthRule::Terminal
    }

    fn type_id(&self) -> &EffectNodeType {
        &self.0
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
                name: "status".into(),
                label: "Status",
                ty: manifold_node_engine::parameters::ParamType::Enum,
                default: ParamValue::Enum(1),
                range: None,
                enum_values: &[
                    "Pending",
                    "Ready A",
                    "Ready B",
                    "Failed",
                    "Prepared Geometry",
                    "Unsupported",
                ],
            }]
        });
        &*PARAMETERS
    }

    fn evaluate(&mut self, _: &mut EffectNodeContext<'_, '_>) {}

    fn source_asset_identity(
        &self,
        params: &manifold_node_engine::exec::effect_node::ParamValues,
    ) -> SourceAssetIdentity<'_> {
        match params.get("status") {
            Some(ParamValue::Enum(0)) => SourceAssetIdentity::Pending,
            Some(ParamValue::Enum(1)) => SourceAssetIdentity::Ready([0xAA; 32]),
            Some(ParamValue::Enum(2)) => SourceAssetIdentity::Ready([0xBB; 32]),
            Some(ParamValue::Enum(3)) => SourceAssetIdentity::Failed("asset decode failed"),
            Some(ParamValue::Enum(4)) => SourceAssetIdentity::PreparedGeometry,
            Some(ParamValue::Enum(5)) => SourceAssetIdentity::Unsupported,
            _ => SourceAssetIdentity::Failed("invalid probe status"),
        }
    }
}

fn asset_source(asset: &str) -> Result<Vec<PhysicsSourceGraph>, String> {
    let mut sources = source().unwrap();
    sources[0].asset_nodes.push(NodeId::new(asset));
    Ok(sources)
}

fn set_status(graph: &mut Graph, node: NodeInstanceId, status: u32) {
    graph
        .set_param(node, "status", ParamValue::Enum(status))
        .unwrap();
}

fn asset_graph() -> (
    Graph,
    [(NodeId, NodeInstanceId); 4],
    NodeInstanceId,
    NodeInstanceId,
) {
    let mut graph = Graph::new();
    let first_fluid = graph.add_node(Box::new(SourceObserver(
        0,
        EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
    )));
    let first_asset = graph.add_node(Box::new(AssetProbe(EffectNodeType::new(
        "node.asset_probe",
    ))));
    let second_fluid = graph.add_node(Box::new(SourceObserver(
        1,
        EffectNodeType::new(manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID),
    )));
    let second_asset = graph.add_node(Box::new(AssetProbe(EffectNodeType::new(
        "node.asset_probe",
    ))));
    let nodes = [
        (NodeId::new("c0.fluid"), first_fluid),
        (NodeId::new("c0.asset"), first_asset),
        (NodeId::new("c1.fluid"), second_fluid),
        (NodeId::new("c1.asset"), second_asset),
    ];
    (graph, nodes, first_asset, second_asset)
}

#[test]
fn asset_observations_are_card_scoped_deduplicated_and_content_sensitive() {
    OBSERVED.with(|observed| *observed.borrow_mut() = [None, None]);
    PUBLICATIONS.with(|count| *count.borrow_mut() = [0, 0]);
    let (mut graph, nodes, first_asset, _second_asset) = asset_graph();
    let mut first_state = PhysicsSourceState::default();
    let mut second_state = PhysicsSourceState::default();
    let first_map = &nodes[..];
    let second_map = &nodes[..];

    first_state.apply_prepared(&mut graph, first_map, "c0.", asset_source("asset"));
    second_state.apply_prepared(&mut graph, second_map, "c1.", asset_source("asset"));
    let ready_a = observed(0);
    assert_eq!(ready_a, observed(1), "same content has the same identity");
    first_state.observe_assets(&mut graph);
    PUBLICATIONS.with(|count| {
        assert_eq!(
            *count.borrow(),
            [1, 1],
            "unchanged ready assets do not republish"
        )
    });

    set_status(&mut graph, first_asset, 0);
    first_state.observe_assets(&mut graph);
    assert!(
        observed(0)
            .unwrap_err()
            .contains("asset 'asset': Source asset is still loading")
    );
    assert_eq!(
        observed(1),
        ready_a,
        "pending state stays scoped to one card"
    );
    first_state.observe_assets(&mut graph);
    PUBLICATIONS.with(|count| assert_eq!(*count.borrow(), [2, 1]));

    set_status(&mut graph, first_asset, 3);
    first_state.observe_assets(&mut graph);
    assert!(
        observed(0)
            .unwrap_err()
            .contains("asset 'asset': asset decode failed")
    );
    assert_eq!(
        observed(1),
        ready_a,
        "failed state stays scoped to one card"
    );
    first_state.observe_assets(&mut graph);
    PUBLICATIONS.with(|count| assert_eq!(*count.borrow(), [3, 1]));

    set_status(&mut graph, first_asset, 5);
    first_state.observe_assets(&mut graph);
    assert!(
        observed(0)
            .unwrap_err()
            .contains("asset 'asset': This source cannot yet validate recorded takes")
    );
    assert_eq!(
        observed(1),
        ready_a,
        "unsupported state stays scoped to one card"
    );

    set_status(&mut graph, first_asset, 4);
    first_state.observe_assets(&mut graph);
    let prepared = observed(0);
    assert!(prepared.is_ok());
    assert_ne!(
        prepared, ready_a,
        "prepared geometry has its own identity tag"
    );

    set_status(&mut graph, first_asset, 2);
    first_state.observe_assets(&mut graph);
    let ready_b = observed(0);
    assert!(ready_b.is_ok());
    assert_ne!(ready_b, ready_a, "content changes alter the identity");
    assert_eq!(observed(1), ready_a);

    set_status(&mut graph, first_asset, 1);
    first_state.observe_assets(&mut graph);
    assert_eq!(
        observed(0),
        ready_a,
        "restoring Ready A restores its fingerprint"
    );
}

#[test]
fn asset_rebuild_reads_fresh_status_and_missing_targets_fail_explicitly() {
    OBSERVED.with(|observed| *observed.borrow_mut() = [None, None]);
    PUBLICATIONS.with(|count| *count.borrow_mut() = [0, 0]);
    let (mut graph, nodes, first_asset, _second_asset) = asset_graph();
    let first_map = &nodes[..];
    let mut state = PhysicsSourceState::default();
    state.apply_prepared(&mut graph, first_map, "c0.", asset_source("asset"));
    let ready_a = observed(0);

    set_status(&mut graph, first_asset, 0);
    let mut rebuilt = PhysicsSourceState::default();
    rebuilt.apply_prepared(&mut graph, first_map, "c0.", asset_source("asset"));
    assert!(
        observed(0)
            .unwrap_err()
            .contains("asset 'asset': Source asset is still loading")
    );
    assert_ne!(
        observed(0),
        ready_a,
        "rebuild does not inherit stale readiness"
    );

    set_status(&mut graph, first_asset, 1);
    rebuilt.observe_assets(&mut graph);
    assert_eq!(observed(0), ready_a);

    let mut missing = source().unwrap();
    missing[0].asset_nodes.push(NodeId::new("missing"));
    rebuilt.apply_prepared(&mut graph, first_map, "c0.", Ok(missing));
    assert!(
        observed(0)
            .unwrap_err()
            .contains("asset source 'missing' is absent from the installed graph")
    );
}
