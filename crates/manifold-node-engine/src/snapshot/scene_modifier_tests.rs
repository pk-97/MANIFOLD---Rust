use super::GraphSnapshot;
use manifold_core::effect_graph_def::EffectGraphDef;

#[test]
fn scene_modifier_snapshot_keeps_authored_nodes_in_an_incomplete_draft() {
    let mut def: EffectGraphDef = serde_json::from_value(serde_json::json!({
        "version":3, "presetMetadata":{"id":"draft","displayName":"Draft",
            "category":"Geometry","oscPrefix":"draft","params":[],"bindings":[],
            "sceneModifier":{"schemaVersion":1,"singleton":false,"enabledParam":"enabled"}},
        "nodes":[{"id":37,"nodeId":"authored","typeId":"node.value","handle":"value",
            "params":{"value":{"type":"Float","value":0.4}}}], "wires":[]
    }))
    .unwrap();
    // This recipe is deliberately incomplete: the editor must display it
    // without attempting render admission or injecting generated host nodes.
    let snapshot = GraphSnapshot::from_def(&def).expect("authored draft remains editable");
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].id, 37);
    assert_eq!(snapshot.nodes[0].node_id.as_str(), "authored");
    def.preset_metadata.as_mut().unwrap().scene_modifier = None;
    def.scene_modifiers = serde_json::from_value(serde_json::json!([{
        "id":"modifier","scene":{"scope":[],"node":"missing-scene"},"targets":"allObjects",
        "graph":{"version":3,"nodes":[],"wires":[]}
    }]))
    .unwrap();
    let snapshot = GraphSnapshot::from_def(&def).expect("host draft remains editable");
    assert_eq!(snapshot.nodes.len(), 1);
    assert_eq!(snapshot.nodes[0].node_id.as_str(), "authored");
}
