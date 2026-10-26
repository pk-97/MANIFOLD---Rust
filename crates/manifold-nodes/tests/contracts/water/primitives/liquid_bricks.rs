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
}
