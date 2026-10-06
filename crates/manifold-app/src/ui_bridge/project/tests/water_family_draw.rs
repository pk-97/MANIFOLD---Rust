//! Deliberate-run GPU proof that Add Water's four physical outputs draw.
//!
//! This stays out of the normal CPU suite. Peter runs it with the
//! journey-proofs feature on macOS through the GPU queue.

#[cfg(all(feature = "journey-proofs", target_os = "macos"))]
mod gpu {
    use super::super::*;
    use crate::content_command::ContentCommand;
    use manifold_core::LayerId;
    use manifold_renderer::node_graph::scene_vm::{SceneObjectKnownRow, SceneObjectVm, SceneVm};
    use std::path::{Path, PathBuf};

    const W: u32 = 640;
    const H: u32 = 360;

    fn scene_rows(project: &Project, layer_id: &LayerId) -> Vec<SceneObjectKnownRow> {
        let def = effective_def(project, layer_id);
        SceneVm::from_def(&def)
            .expect("scene VM")
            .objects
            .into_iter()
            .filter_map(|object| match object {
                SceneObjectVm::Known(row) => Some(*row),
                SceneObjectVm::Custom { .. } => None,
            })
            .collect()
    }

    fn queue_scene_param(
        content: &mut crate::content_thread::ContentThread,
        layer_id: &LayerId,
        scope_path: Vec<u32>,
        node_doc_id: u32,
        param_id: &str,
        value: f32,
    ) {
        let mut project = content.engine.project().expect("content project").clone();
        let (_, state, mut ui, mut selection, mut active_layer, mut prefs) = dispatch_harness();
        let (tx, rx) = crossbeam_channel::unbounded();
        dispatch_project(
            &ProjectAction::SceneSetupParamChanged(
                layer_id.clone(),
                scope_path,
                node_doc_id,
                param_id.to_owned(),
                value,
            ),
            &mut project,
            &tx,
            &state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut prefs,
        );
        let Some(command) = rx.try_iter().next() else {
            // The dispatch layer suppresses an epsilon no-op.
            return;
        };
        assert!(
            matches!(&command, ContentCommand::ExecuteOnContent(_)),
            "scene visibility writes use ExecuteOnContent"
        );
        assert!(!content.handle_command(command));
        assert!(
            content.graph_edit_diagnostic.is_none(),
            "scene visibility write was rejected: {:?}",
            content.graph_edit_diagnostic
        );
        assert!(
            content.editing_service.can_undo(),
            "visibility write must pass through EditingService"
        );
    }

    fn readback_raw(
        content: &crate::content_thread::ContentThread,
        device: &std::sync::Arc<manifold_gpu::GpuDevice>,
    ) -> Vec<u8> {
        content.content_pipeline.wait_for_render_complete();
        manifold_renderer::headless_readback::readback_raw_halves(
            device,
            content.content_pipeline.export_output_texture(),
            W,
            H,
        )
    }

    fn write_png(path: &Path, raw: &[u8]) {
        let rgba: Vec<u8> = raw
            .chunks_exact(2)
            .enumerate()
            .map(|(index, half)| {
                let value = half::f16::from_bits(u16::from_le_bytes([half[0], half[1]])).to_f32();
                let channel = index % 4;
                if channel == 3 {
                    (value.clamp(0.0, 1.0) * 255.0).round() as u8
                } else {
                    manifold_renderer::headless_readback::linear_to_srgb8(value)
                }
            })
            .collect();
        std::fs::write(
            path,
            manifold_renderer::headless_readback::encode_rgba8_png(&rgba, W, H),
        )
        .unwrap_or_else(|error| panic!("write {}: {error}", path.display()));
    }

    fn assert_finite(raw: &[u8], label: &str) {
        for half in raw.chunks_exact(2) {
            let value = half::f16::from_bits(u16::from_le_bytes([half[0], half[1]])).to_f32();
            assert!(value.is_finite(), "{label} contains a non-finite pixel");
        }
    }

    fn difference_from_hidden(raw: &[u8], hidden: &[u8]) -> (usize, f64) {
        assert_eq!(raw.len(), hidden.len());
        let mut pixels = 0;
        let mut energy = 0.0f64;
        for (current, baseline) in raw.chunks_exact(8).zip(hidden.chunks_exact(8)) {
            let mut pixel_energy = 0.0f64;
            for channel in 0..3 {
                let current = half::f16::from_bits(u16::from_le_bytes([
                    current[channel * 2],
                    current[channel * 2 + 1],
                ]))
                .to_f32();
                let baseline = half::f16::from_bits(u16::from_le_bytes([
                    baseline[channel * 2],
                    baseline[channel * 2 + 1],
                ]))
                .to_f32();
                pixel_energy += f64::from((current - baseline).abs());
            }
            if pixel_energy > 1.0e-5 {
                pixels += 1;
                energy += pixel_energy;
            }
        }
        (pixels, energy)
    }

    fn set_family_visibility(
        content: &mut crate::content_thread::ContentThread,
        layer_id: &LayerId,
        water: &SceneObjectKnownRow,
        children: &[SceneObjectKnownRow],
        target: usize,
    ) {
        // The recipe-authored shared gate is the family switch. Local visible
        // writes then isolate Water itself or exactly one look.
        queue_scene_param(
            content,
            layer_id,
            water.visible_addr.scope_path.clone(),
            water.object_node_id,
            "parent_visible",
            1.0,
        );
        queue_scene_param(
            content,
            layer_id,
            water.visible_addr.scope_path.clone(),
            water.object_node_id,
            "visible",
            if target == 0 { 1.0 } else { 0.0 },
        );
        for (index, child) in children.iter().enumerate() {
            queue_scene_param(
                content,
                layer_id,
                child.visible_addr.scope_path.clone(),
                child.object_node_id,
                "visible",
                if target == index + 1 { 1.0 } else { 0.0 },
            );
        }
    }

    #[test]
    fn water_family_four_outputs_draw() {
        let output_dir = PathBuf::from("target/journey-proofs/water-family-four-outputs");
        std::fs::create_dir_all(&output_dir).expect("water-family output directory");

        let (mut project, layer_id, render_scene_node_id) =
            super::super::water_family::water_project();
        let initial_objects = objects_param(&project, &layer_id, render_scene_node_id) as usize;
        project.settings.output_width = W as i32;
        project.settings.output_height = H as i32;
        project.timeline.layers[0]
            .clips
            .push(manifold_core::clip::TimelineClip::new_generator(
                manifold_core::Beats::ZERO,
                manifold_core::Beats(128.0),
            ));

        let (_, state, mut ui, mut selection, mut active_layer, mut prefs) = dispatch_harness();
        let (tx, rx) = crossbeam_channel::unbounded();
        dispatch_project(
            &ProjectAction::SceneSetupAddFluid(layer_id.clone(), render_scene_node_id),
            &mut project,
            &tx,
            &state,
            &mut ui,
            &mut selection,
            &mut active_layer,
            &mut prefs,
        );
        let add = rx.try_recv().expect("Add Water queued a content edit");
        assert!(matches!(&add, ContentCommand::ExecuteSelecting(_, _)));

        let mut content = crate::headless_harness::headless_content_thread(project, W, H);
        let (state_tx, _state_rx) = crossbeam_channel::unbounded();
        content.handle_command(add);
        crate::scene_modifier_journey::warm_project(&mut content, &state_tx);

        // Let the bounded fixture capture actual liquid and look populations,
        // then freeze the solver while the four outputs are isolated.
        content.handle_command(ContentCommand::Play);
        for _ in 0..120 {
            content.tick_frame(&state_tx);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        content.handle_command(ContentCommand::Pause);
        content.tick_frame(&state_tx);

        let device = std::sync::Arc::clone(
            content
                .content_pipeline
                .native_gpu_for_tests()
                .expect("native journey device"),
        );
        let project_after = content.engine.project().expect("content project");
        let def = effective_def(project_after, &layer_id);
        let rows = scene_rows(project_after, &layer_id);
        let water = rows
            .iter()
            .find(|row| row.is_group && row.liquid_domain.is_some() && row.index >= initial_objects)
            .cloned()
            .expect("Add Water family parent row");
        let children: Vec<_> = rows
            .iter()
            .filter(|row| {
                row.parent_group_id == Some(water.object_node_id)
                    && (initial_objects..initial_objects + 4).contains(&row.index)
            })
            .cloned()
            .collect();
        assert_eq!(children.len(), 3, "Water has Foam, Spray and Bubbles rows");
        assert!(children.iter().all(|row| row.look_mesh.is_some()));

        let mut family_rows = vec![water.clone()];
        family_rows.extend(children.iter().cloned());
        family_rows.sort_by_key(|row| row.index);
        assert_eq!(
            family_rows.iter().map(|row| row.index).collect::<Vec<_>>(),
            (initial_objects..initial_objects + 4).collect::<Vec<_>>(),
            "Add Water reserves four contiguous physical slots"
        );
        let group_id = water.group_node_id.expect("Water group node");
        for (offset, port) in ["object", "object_1", "object_2", "object_3"]
            .into_iter()
            .enumerate()
        {
            assert!(
                def.wires.iter().any(|wire| {
                    wire.from_node == group_id
                        && wire.from_port == port
                        && wire.to_node == render_scene_node_id
                        && wire.to_port == format!("object_{}", initial_objects + offset)
                }),
                "Water output {port} reaches its physical render slot"
            );
        }
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flattened Add Water graph");
        let render_stable = &def.nodes.iter().find(|node| node.id == render_scene_node_id).unwrap().node_id;
        let flat_render = flat.nodes.iter().find(|node| &node.node_id == render_stable).unwrap().id;
        for row in &family_rows {
            let object = flat
                .nodes
                .iter()
                .find(|node| node.node_id == row.object)
                .expect("family render object survives flattening");
            assert!(
                flat.wires.iter().any(|wire| {
                    wire.from_node == object.id
                        && wire.to_node == flat_render
                        && wire.to_port == format!("object_{}", row.index)
                }),
                "family object reaches physical render slot {}",
                row.index
            );
        }

        // The shipped family shares this camera and volume. Hide it through
        // its authored gate so it cannot occlude the newly inserted outputs.
        for original in rows.iter().filter(|row| row.is_group && row.liquid_domain.is_some()
            && row.index < initial_objects) {
            queue_scene_param(&mut content, &layer_id, original.visible_addr.scope_path.clone(),
                original.object_node_id, "parent_visible", 0.0);
        }

        // All-family-hidden is the comparison frame for every physical slot.
        queue_scene_param(
            &mut content,
            &layer_id,
            water.visible_addr.scope_path.clone(),
            water.object_node_id,
            "parent_visible",
            0.0,
        );
        queue_scene_param(
            &mut content,
            &layer_id,
            water.visible_addr.scope_path.clone(),
            water.object_node_id,
            "visible",
            0.0,
        );
        for child in &children {
            queue_scene_param(
                &mut content,
                &layer_id,
                child.visible_addr.scope_path.clone(),
                child.object_node_id,
                "visible",
                0.0,
            );
        }
        content.tick_frame(&state_tx);
        let hidden = readback_raw(&content, &device);
        assert_finite(&hidden, "all-family-hidden baseline");
        write_png(&output_dir.join("hidden.png"), &hidden);

        let mut ordered_children = children;
        ordered_children.sort_by_key(|row| row.index);
        let labels = ["water", "foam", "spray", "bubbles"];
        for (target, label) in labels.into_iter().enumerate() {
            set_family_visibility(&mut content, &layer_id, &water, &ordered_children, target);
            content.tick_frame(&state_tx);
            let raw = readback_raw(&content, &device);
            assert_finite(&raw, label);
            let (pixels, energy) = difference_from_hidden(&raw, &hidden);
            write_png(&output_dir.join(format!("{label}.png")), &raw);
            println!("Water family {label}: {pixels} changed pixels, RGB energy {energy:.6}");
            assert!(
                pixels > 0 && energy > 1.0e-5,
                "{label} did not draw against hidden baseline"
            );
        }
    }
}
