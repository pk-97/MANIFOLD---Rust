//! Renderer-owned catalog contract for element-space resolution.

use manifold_node_engine::freeze::install::fuse_canonical_def;
use manifold_node_engine::freeze::space::{space_of, resolve_output_spaces, ElementSpace};
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_node_engine::persistence::PrimitiveRegistry;

#[cfg(test)]
mod tests {
    use crate::freeze::space::*;

    /// Camera Sky declares full-canvas scale and Over defaults to the canvas:
    /// both resolve to one space, so the pair still fuses into one kernel.
    #[test]
    fn full_canvas_scale_is_the_canvas_space() {
        let json = r#"{
            "version": 1, "name": "CameraSkyOverSpace", "nodes": [
                { "id": 0, "typeId": "system.source", "nodeId": "source" },
                { "id": 1, "typeId": "node.free_camera", "nodeId": "cam" },
                { "id": 2, "typeId": "node.camera_sky", "nodeId": "sky" },
                { "id": 3, "typeId": "node.over", "nodeId": "over" },
                { "id": 4, "typeId": "system.final_output", "nodeId": "final_output" }
            ], "wires": [
                { "fromNode": 0, "fromPort": "out", "toNode": 2, "toPort": "sky" },
                { "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "camera" },
                { "fromNode": 0, "fromPort": "out", "toNode": 3, "toPort": "top" },
                { "fromNode": 2, "fromPort": "out", "toNode": 3, "toPort": "bottom" },
                { "fromNode": 3, "fromPort": "out", "toNode": 4, "toPort": "in" }
            ]
        }"#;
        let def: EffectGraphDef = serde_json::from_str(json).expect("parse fixture graph");
        let registry = PrimitiveRegistry::with_builtin();
        let spaces = resolve_output_spaces(&def, &registry).expect("fixture builds");
        assert_eq!(space_of(Some(&spaces), 2, "out"), ElementSpace::Canvas);
        assert_eq!(space_of(Some(&spaces), 3, "out"), ElementSpace::Canvas);
        let fused = fuse_canonical_def(&def, &registry).expect("camera_sky + over is one fusable region");
        assert_eq!(fused.def.nodes.iter().filter(|n| n.type_id == "node.wgsl_compute").count(), 1);
    }
}
