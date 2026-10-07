/// Build the stock MaskBlob from the resolved BlobTrackingV2 detector group.
/// The detector body is cloned from the catalog entry at load time; only the
/// mask consumer tail is authored here. This keeps detector controls and
/// wiring coupled to the source preset while each MaskBlob instance still gets
/// independent primitive state after the normal graph loader clones the def.
pub(super) fn synthesize_mask_blob_json(blob_tracking_json: &str) -> Result<String, String> {
    use serde_json::{Value, json};

    fn node_id(node: &Value) -> Option<&str> {
        node.get("nodeId").and_then(Value::as_str)
    }

    fn collect_node_ids(nodes: &[Value], ids: &mut std::collections::HashSet<String>) {
        for node in nodes {
            if let Some(id) = node_id(node) {
                ids.insert(id.to_owned());
            }
            if let Some(group_nodes) = node
                .get("group")
                .and_then(|group| group.get("nodes"))
                .and_then(Value::as_array)
            {
                collect_node_ids(group_nodes, ids);
            }
        }
    }

    let mut source: Value = serde_json::from_str(blob_tracking_json)
        .map_err(|error| format!("BlobTrackingV2 JSON is invalid: {error}"))?;
    let source_metadata = source
        .get("presetMetadata")
        .cloned()
        .and_then(|value| value.as_object().cloned())
        .ok_or_else(|| "BlobTrackingV2 has no presetMetadata object".to_owned())?;
    let source_nodes = source
        .get("nodes")
        .and_then(Value::as_array)
        .ok_or_else(|| "BlobTrackingV2 has no nodes array".to_owned())?;
    let mut detector = source_nodes
        .iter()
        .find(|node| {
            node.get("typeId").and_then(Value::as_str) == Some("group")
                && node_id(node) == Some("Blob Detection")
        })
        .cloned()
        .ok_or_else(|| "BlobTrackingV2 has no Blob Detection group".to_owned())?;
    detector
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| "Blob Detection group node has no numeric id".to_owned())?;
    // Root group ids share the top-level namespace with the mask tail. Keep
    // the imported detector at the reserved id used by the composed graph so
    // an edited source id cannot collide with tail nodes 7..15.
    const DETECTOR_ROOT_ID: u64 = 19;
    detector["id"] = json!(DETECTOR_ROOT_ID);
    let detector_group = detector
        .get_mut("group")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "Blob Detection node has no group body".to_owned())?;
    let detector_nodes = detector_group
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| "Blob Detection group has no nodes array".to_owned())?;
    let detect_id = detector_nodes
        .iter()
        .find(|node| node_id(node) == Some("detect"))
        .and_then(|node| node.get("id"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "Blob Detection group has no detect node".to_owned())?;
    let tracker_id = detector_nodes
        .iter()
        .find(|node| node_id(node) == Some("tracker"))
        .and_then(|node| node.get("id"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "Blob Detection group has no tracker node".to_owned())?;
    let output_id = detector_nodes
        .iter()
        .find(|node| node.get("typeId").and_then(Value::as_str) == Some("system.group_output"))
        .and_then(|node| node.get("id"))
        .and_then(Value::as_u64)
        .ok_or_else(|| "Blob Detection group has no group output node".to_owned())?;

    {
        let detector_interface = detector_group
            .get_mut("interface")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "Blob Detection group has no interface".to_owned())?;
        let outputs = detector_interface
            .get_mut("outputs")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "Blob Detection group interface has no outputs".to_owned())?;
        for (name, port_type) in [
            ("labels", "Texture2D"),
            ("valid", "Scalar(F32)"),
            ("tracks", ""),
        ] {
            if !outputs
                .iter()
                .any(|port| port.get("name").and_then(Value::as_str) == Some(name))
            {
                outputs.push(json!({"name": name, "portType": port_type}));
            }
        }
    }
    let detector_wires = detector_group
        .get_mut("wires")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "Blob Detection group has no wires array".to_owned())?;
    for (from_node, from_port, to_port) in [
        (detect_id, "labels", "labels"),
        (detect_id, "valid", "valid"),
        (tracker_id, "tracks", "tracks"),
    ] {
        if !detector_wires.iter().any(|wire| {
            wire.get("fromNode").and_then(Value::as_u64) == Some(from_node)
                && wire.get("fromPort").and_then(Value::as_str) == Some(from_port)
                && wire.get("toNode").and_then(Value::as_u64) == Some(output_id)
                && wire.get("toPort").and_then(Value::as_str) == Some(to_port)
        }) {
            detector_wires.push(json!({
                "fromNode": from_node,
                "fromPort": from_port,
                "toNode": output_id,
                "toPort": to_port,
            }));
        }
    }

    let mut detector_ids = std::collections::HashSet::new();
    collect_node_ids(&detector_nodes, &mut detector_ids);
    let source_params = source_metadata
        .get("params")
        .and_then(Value::as_array)
        .ok_or_else(|| "BlobTrackingV2 metadata has no params array".to_owned())?;
    let source_bindings = source_metadata
        .get("bindings")
        .and_then(Value::as_array)
        .ok_or_else(|| "BlobTrackingV2 metadata has no bindings array".to_owned())?;
    let detector_bindings: Vec<Value> = source_bindings
        .iter()
        .filter(|binding| {
            binding
                .get("target")
                .and_then(|target| target.get("nodeId"))
                .and_then(Value::as_str)
                .is_some_and(|id| detector_ids.contains(id))
        })
        .cloned()
        .collect();
    let detector_param_ids: std::collections::HashSet<&str> = detector_bindings
        .iter()
        .filter_map(|binding| binding.get("id").and_then(Value::as_str))
        .collect();
    let mut params: Vec<Value> = source_params
        .iter()
        .filter(|param| {
            param
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| detector_param_ids.contains(id))
        })
        .cloned()
        .collect();
    let mask_tail_params = json!([
        {"id":"shape","name":"Shape","min":0.0,"max":1.0,"defaultValue":0.0,"wholeNumbers":false,"isToggle":false,"isTrigger":false,"formatString":"F2"},
        {"id":"selection","name":"Selection","min":0.0,"max":1.0,"defaultValue":0.0,"wholeNumbers":true,"isToggle":false,"isTrigger":false,"valueLabels":["All","Largest"],"formatString":"F0"},
        {"id":"expand","name":"Expand","min":-32.0,"max":32.0,"defaultValue":0.0,"wholeNumbers":true,"isToggle":false,"isTrigger":false,"formatString":"F0"},
        {"id":"feather","name":"Feather","min":0.0,"max":8.0,"defaultValue":1.0,"wholeNumbers":false,"isToggle":false,"isTrigger":false,"formatString":"F2"},
        {"id":"invert","name":"Invert","min":0.0,"max":1.0,"defaultValue":0.0,"wholeNumbers":false,"isToggle":true,"isTrigger":false,"formatString":"F0"},
        {"id":"amount","name":"Amount","min":0.0,"max":1.0,"defaultValue":1.0,"wholeNumbers":false,"isToggle":false,"isTrigger":false,"formatString":"F2"}
    ]);
    params.extend(mask_tail_params.as_array().unwrap().iter().cloned());
    let mask_tail_bindings = json!([
        {"id":"shape","label":"Shape","defaultValue":0.0,"target":{"kind":"node","nodeId":"region_mask","param":"shape"},"convert":{"type":"Float"}},
        {"id":"selection","label":"Selection","defaultValue":0.0,"target":{"kind":"node","nodeId":"region_mask","param":"selection"},"convert":{"type":"EnumRound"}},
        {"id":"expand","label":"Expand","defaultValue":0.0,"target":{"kind":"node","nodeId":"expand_x","param":"radius"},"convert":{"type":"IntRound"}},
        {"id":"expand","label":"Expand","defaultValue":0.0,"target":{"kind":"node","nodeId":"expand_y","param":"radius"},"convert":{"type":"IntRound"}},
        {"id":"feather","label":"Feather","defaultValue":1.0,"target":{"kind":"node","nodeId":"feather_h","param":"radius"},"convert":{"type":"Float"}},
        {"id":"feather","label":"Feather","defaultValue":1.0,"target":{"kind":"node","nodeId":"feather_v","param":"radius"},"convert":{"type":"Float"}},
        {"id":"invert","label":"Invert","defaultValue":0.0,"target":{"kind":"node","nodeId":"invert","param":"intensity"},"convert":{"type":"Float"}},
        {"id":"amount","label":"Amount","defaultValue":1.0,"target":{"kind":"node","nodeId":"amount","param":"scale"},"convert":{"type":"Float"}}
    ]);
    let mut bindings = detector_bindings;
    bindings.extend(mask_tail_bindings.as_array().unwrap().iter().cloned());

    let tail_nodes = json!([
        {"id":7,"nodeId":"region_mask","typeId":"node.region_mask","handle":"region_mask","params":{"selection":{"type":"Enum","value":0},"shape":{"type":"Float","value":0.0}},"title":"Region Mask"},
        {"id":8,"nodeId":"expand_x","typeId":"node.mask_extrema","handle":"expand_x","params":{"radius":{"type":"Float","value":0.0},"axis":{"type":"Enum","value":0}},"title":"Expand Horizontal"},
        {"id":9,"nodeId":"expand_y","typeId":"node.mask_extrema","handle":"expand_y","params":{"radius":{"type":"Float","value":0.0},"axis":{"type":"Enum","value":1}},"title":"Expand Vertical"},
        {"id":10,"nodeId":"feather_h","typeId":"node.gaussian_blur","handle":"feather_h","params":{"kernel_size":{"type":"Enum","value":1},"axis":{"type":"Enum","value":0},"step":{"type":"Float","value":1.0},"radius_mode":{"type":"Enum","value":2},"radius":{"type":"Float","value":1.0},"address_mode":{"type":"Enum","value":0}},"title":"Feather Horizontal"},
        {"id":11,"nodeId":"feather_v","typeId":"node.gaussian_blur","handle":"feather_v","params":{"kernel_size":{"type":"Enum","value":1},"axis":{"type":"Enum","value":1},"step":{"type":"Float","value":1.0},"radius_mode":{"type":"Enum","value":2},"radius":{"type":"Float","value":1.0},"address_mode":{"type":"Enum","value":0}},"title":"Feather Vertical"},
        {"id":12,"nodeId":"invert","typeId":"node.invert","handle":"invert","params":{"intensity":{"type":"Float","value":0.0}},"title":"Invert"},
        {"id":13,"nodeId":"amount","typeId":"node.scale_offset_image","handle":"amount","params":{"scale":{"type":"Float","value":1.0},"offset":{"type":"Float","value":0.0}},"title":"Amount"},
        {"id":14,"nodeId":"valid_gate","typeId":"node.scale_offset_image","handle":"valid_gate","params":{"scale":{"type":"Float","value":0.0},"offset":{"type":"Float","value":0.0}},"title":"Validity Gate"},
        {"id":15,"nodeId":"final_output","typeId":"system.final_output","handle":"final_output","title":"Output"}
    ]);
    let tail_wires = json!([
        {"fromNode":DETECTOR_ROOT_ID,"fromPort":"labels","toNode":7,"toPort":"labels"},
        {"fromNode":DETECTOR_ROOT_ID,"fromPort":"tracks","toNode":7,"toPort":"tracks"},
        {"fromNode":7,"fromPort":"out","toNode":8,"toPort":"in"},
        {"fromNode":8,"fromPort":"out","toNode":9,"toPort":"in"},
        {"fromNode":9,"fromPort":"out","toNode":10,"toPort":"in"},
        {"fromNode":10,"fromPort":"out","toNode":11,"toPort":"in"},
        {"fromNode":11,"fromPort":"out","toNode":12,"toPort":"in"},
        {"fromNode":12,"fromPort":"out","toNode":13,"toPort":"in"},
        {"fromNode":13,"fromPort":"out","toNode":14,"toPort":"in"},
        {"fromNode":DETECTOR_ROOT_ID,"fromPort":"valid","toNode":14,"toPort":"scale"},
        {"fromNode":14,"fromPort":"out","toNode":15,"toPort":"in"}
    ]);

    let metadata = source
        .get_mut("presetMetadata")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| "BlobTrackingV2 has no mutable presetMetadata object".to_owned())?;
    metadata.insert("id".into(), json!("MaskBlob"));
    metadata.insert("displayName".into(), json!("Mask Blob Detector"));
    metadata.insert("category".into(), json!("Spatial"));
    metadata.insert("oscPrefix".into(), json!("maskBlob"));
    metadata.insert("available".into(), json!(false));
    metadata.insert("params".into(), Value::Array(params));
    metadata.insert("bindings".into(), Value::Array(bindings));
    source["name"] = json!("MaskBlob");
    source["nodes"] = Value::Array(
        std::iter::once(json!({"id":0,"nodeId":"source","typeId":"system.source","handle":"source","title":"Source"}))
            .chain(std::iter::once(detector))
            .chain(tail_nodes.as_array().unwrap().iter().cloned())
            .collect(),
    );
    source["wires"] = Value::Array(
        tail_wires
            .as_array()
            .unwrap()
            .iter()
            .cloned()
            .chain(std::iter::once(
                json!({"fromNode":0,"fromPort":"out","toNode":DETECTOR_ROOT_ID,"toPort":"source"}),
            ))
            .collect(),
    );
    serde_json::to_string(&source)
        .map_err(|error| format!("MaskBlob synthesis failed to serialize: {error}"))
}
