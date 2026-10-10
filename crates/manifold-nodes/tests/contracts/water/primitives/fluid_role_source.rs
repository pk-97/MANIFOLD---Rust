mod geometry;

mod tests {
    use crate::contracts::water::primitives::fluid_role_source::geometry;
    use std::borrow::Cow;
    use std::sync::Arc;
    use manifold_nodes_water::primitives::fluid_role_source::FluidRoleSource;
    use manifold_nodes_water::testkit::fluid_role_source::{test_slots, settle_inputs};
    use manifold_node_engine::exec::backend::{Backend, MockBackend};
    use manifold_node_engine::exec::effect_node::ParamValues;
    use manifold_node_engine::exec::execution_plan::ResourceId;
    use manifold_node_engine::ports::PortType;
    use manifold_node_engine::parameters::{ParamValue, TableData};
    use manifold_node_engine::scene::transform::Transform;

    #[test]
    fn scene_physics_fluid_role_source_compound_recooks_parts_but_reuses_body_pose() {
        let (path, compound) = geometry::tests::write_two_material_cube_fixture();
        let mut backend = MockBackend::new();
        let (transform_slot, output_slot) = test_slots(&mut backend);
        let part_zero = backend.acquire(ResourceId(2), PortType::Transform, None, (0, 0));
        let part_one = backend.acquire(ResourceId(3), PortType::Transform, None, (0, 0));
        let source = backend.acquire(ResourceId(4), PortType::Transform, None, (0, 0));
        backend.set_transform(part_zero, compound.part_transforms[0]);
        backend.set_transform(part_one, compound.part_transforms[1]);
        backend.set_transform(
            source,
            Transform {
                pos: [0.0, 2.0, 0.0],
                ..Transform::default()
            },
        );
        let inputs = [
            ("transform", transform_slot),
            ("part_0", part_zero),
            ("part_1", part_one),
            ("source_transform", source),
        ];
        let mut params = ParamValues::default();
        params.insert(
            Cow::Borrowed("path"),
            ParamValue::String(Arc::new(path.to_string_lossy().into_owned())),
        );
        params.insert(Cow::Borrowed("geometry"), ParamValue::Enum(1));
        params.insert(Cow::Borrowed("recenter"), ParamValue::Bool(false));
        params.insert(
            Cow::Borrowed("compound_materials"),
            ParamValue::Table(Arc::new(
                TableData::new(vec![vec![0.0, 0.0], vec![1.0, 1.0]]).unwrap(),
            )),
        );
        let mut primitive = FluidRoleSource::new();
        let first = settle_inputs(&mut primitive, &mut backend, &inputs, output_slot, &params);
        let min_y = first.geometry.meshes[0]
            .vertices
            .iter()
            .map(|v| v[1])
            .fold(f32::INFINITY, f32::min);
        assert_eq!(
            min_y, 1.5,
            "source transform applies to the assembled compound"
        );
        backend.set_transform(
            transform_slot,
            Transform {
                pos: [3.0, 0.0, 0.0],
                ..Transform::default()
            },
        );
        let moved_body = settle_inputs(&mut primitive, &mut backend, &inputs, output_slot, &params);
        assert!(Arc::ptr_eq(&first.geometry, &moved_body.geometry));
        assert_eq!(moved_body.transform.pos, [3.0, 0.0, 0.0]);
        for (slot, mut transform) in [
            (part_zero, compound.part_transforms[0]),
            (part_one, compound.part_transforms[1]),
        ] {
            transform.pos[0] += 1.0;
            backend.set_transform(slot, transform);
        }
        let moved_parts =
            settle_inputs(&mut primitive, &mut backend, &inputs, output_slot, &params);
        assert!(!Arc::ptr_eq(&first.geometry, &moved_parts.geometry));
        let min_x = moved_parts.geometry.meshes[0]
            .vertices
            .iter()
            .map(|v| v[0])
            .fold(f32::INFINITY, f32::min);
        assert_eq!(min_x, 0.5, "child transforms alter the prepared geometry");
        manifold_fluids::validate_mesh(&moved_parts.geometry.meshes[0]).unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
