mod tests {
use manifold_nodes_water::testkit::liquid_extents::walk;
#[test]
fn fluid_bricks_preset_extents_cover_dense_storage_and_schedule() {
    use manifold_nodes_water::presets::gpu_flip::{WaterScene, render_def};
    for resolution in [64, 128] {
        for scale in [1, 2, 4] {
            let def = render_def(WaterScene::dam_break(resolution).with_surface_scale(scale));
            for frozen in [false, true] {
                walk(&def, frozen).unwrap_or_else(|error| {
                    panic!("resolution {resolution}, scale {scale}, frozen {frozen}: {error}")
                });
            }
        }
    }
}

#[test]
fn fluid_bricks_dense_fusion_is_independent_of_optional_schedules() {
    use manifold_node_engine::persistence::PrimitiveRegistry;
    use manifold_node_engine::freeze::install::fuse_generator_view;
    use manifold_nodes_water::presets::gpu_flip::{WaterScene, render_def};

    fn remove_schedules(value: &mut serde_json::Value) -> usize {
        match value {
            serde_json::Value::Object(object) => {
                let mut removed = 0;
                if let Some(wires) = object.get_mut("wires").and_then(|v| v.as_array_mut()) {
                    let before = wires.len();
                    wires.retain(|wire| wire["toPort"] != "bricks");
                    removed += before - wires.len();
                }
                removed + object.values_mut().map(remove_schedules).sum::<usize>()
            }
            serde_json::Value::Array(values) => values.iter_mut().map(remove_schedules).sum(),
            _ => 0,
        }
    }

    let def = render_def(WaterScene::dam_break(16));
    let registry = PrimitiveRegistry::with_builtin();
    let source = |def: &manifold_core::effect_graph_def::EffectGraphDef| {
        let fused = fuse_generator_view(def, &registry).expect("surface chain must fuse");
        let source = fused
            .def
            .nodes
            .iter()
            .filter_map(|node| node.wgsl_source.as_ref())
            .find(|source| source.contains("n0_passes") && source.contains("n1_cell_size"))
            .expect("smooth and clamp must share a fused kernel")
            .clone();
        let module = naga::front::wgsl::parse_str(&source).unwrap();
        naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        )
        .validate(&module)
        .expect("dense fused kernel validates");
        let storage_inputs = module.global_variables.iter().filter(|(_, global)| {
            matches!(global.space, naga::AddressSpace::Storage { access } if access == naga::StorageAccess::LOAD)
        }).count();
        assert_eq!(storage_inputs, 2, "only levelset and solid are data inputs");
        assert!(
            !source.contains("params.n0_brick_pass") && !source.contains("params.n1_brick_pass"),
            "dense fusion must not depend on standalone pass uniforms"
        );
        source
    };
    let scheduled = source(&def);
    let mut unwired = serde_json::to_value(&def).unwrap();
    assert!(
        remove_schedules(&mut unwired) > 0,
        "fixture must wire brick schedules"
    );
    let unwired = serde_json::from_value(unwired).unwrap();
    assert_eq!(
        scheduled,
        source(&unwired),
        "schedule presence must not change the dense kernel"
    );
}
}
