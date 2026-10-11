//! Load migrations for liquid clocks, frame cursors and retained solver outputs.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};

/// Add explicit interval and clock telemetry wires to pre-clock liquid graphs,
/// preserving authored wires and all export settings. Runs once at installation.
fn wire_liquid_intervals(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let mut domains = std::collections::BTreeMap::new();
    for node in &def.nodes {
        if matches!(node.type_id.as_str(), GPU_FLIP_DOMAIN_TYPE_ID | MATTER_DOMAIN_TYPE_ID) {
            domains.insert(node.id, node.id);
        }
    }
    // Follow only solver boundaries, never arbitrary graph ancestry.
    for _ in 0..3 {
        for node in &def.nodes {
            if !matches!(node.type_id.as_str(), "node.liquid_state" | "node.matter_state" | "node.gpu_flip_step") { continue; }
            let source = def.wires.iter().filter(|wire| wire.to_node == node.id)
                .filter_map(|wire| domains.get(&wire.from_node).copied()).next();
            if let Some(source) = source { domains.insert(node.id, source); }
        }
    }
    let mut changed = false;
    for node in &def.nodes {
        let clock_ports: &[(&str, &str)] = match node.type_id.as_str() {
            "node.gpu_flip_step" => &[("clock_obstacles", "clock_obstacles"), ("clock_sources", "clock_sources"), ("clock_obstacle_count", "clock_obstacle_count"), ("clock_source_count", "clock_source_count"), ("initial_obstacle_speed", "initial_obstacle_speed"), ("live_hits", "live_hits"), ("live_hit_count", "live_hit_count"), ("limit_interval", "limit_interval")],
            "node.liquid_state" => &[("dropped_seconds", "dropped_seconds")],
            "node.matter_state" => &[("target_time", "target_time"), ("simulation_time", "simulation_time"), ("step_cap_hit", "step_cap_hit"), ("dropped_seconds", "dropped_seconds")],
            _ => &[],
        };
        let intervals = manifold_water_liquid::clock::INTERVAL_DURATION_INPUTS.iter()
            .filter(|(type_id, _)| *type_id == node.type_id.as_str())
            .map(|&(_, input)| ("interval_duration", input));
        let source = def.wires.iter().filter(|wire| wire.to_node == node.id)
            .find_map(|wire| domains.get(&wire.from_node).copied());
        if let Some(source) = source {
            for (output, input) in intervals.chain(clock_ports.iter().copied()) {
                if def.wires.iter().any(|wire| wire.to_node == node.id && wire.to_port == input) { continue; }
                def.wires.push(EffectGraphWire { from_node: source, from_port: output.into(), to_node: node.id, to_port: input.into() });
                changed = true;
            }
        }
        if matches!(node.type_id.as_str(), "node.gpu_flip_step" | "node.liquid_frame") {
            let state = def.wires.iter().filter(|w| w.to_node == node.id && w.to_port == "particles")
                .find_map(|w| def.nodes.iter().find(|n| n.id == w.from_node && n.type_id == "node.liquid_state"));
            if let Some(state) = state {
                if node.type_id == "node.gpu_flip_step" && !def.wires.iter().any(|w| w.to_node == node.id && w.to_port == "retired_max_speed") {
                    def.wires.push(EffectGraphWire { from_node: state.id, from_port: "retired_max_speed".into(), to_node: node.id, to_port: "retired_max_speed".into() });
                    changed = true;
                }
                if !def.wires.iter().any(|w| w.to_node == node.id && w.to_port == "identity") {
                    def.wires.push(EffectGraphWire { from_node: state.id, from_port: "identity".into(), to_node: node.id, to_port: "identity".into() });
                    changed = true;
                }
                if node.type_id == "node.gpu_flip_step" && !def.wires.iter().any(|w| w.to_node == state.id && w.to_port == "identity_in") {
                    def.wires.push(EffectGraphWire { from_node: node.id, from_port: "identity_out".into(), to_node: state.id, to_port: "identity_in".into() });
                    changed = true;
                }
            }
        }
        if node.type_id == "node.liquid_state" && !def.wires.iter().any(|w| w.to_node == node.id && w.to_port == "clock_status_in") {
            let step = def.wires.iter().filter(|w| w.to_node == node.id && w.to_port == "in")
                .find_map(|w| def.nodes.iter().find(|n| n.id == w.from_node && n.type_id == "node.gpu_flip_step"));
            if let Some(step) = step {
                def.wires.push(EffectGraphWire { from_node: step.id, from_port: "clock_status".into(), to_node: node.id, to_port: "clock_status_in".into() });
                changed = true;
            }
        }
    }
    changed
}

/// Give saved GPU FLIP frames the presentation cursor
/// (GPU_FLIP_DISPLAY_HISTORY_DESIGN.md section 3.4 (Cursor)), after
/// flattening. The frame's clock is the node feeding its `epoch`, never the
/// first incoming wire; only a GPU FLIP domain there gets `display_cursor`
/// and `dropped_seconds` wired into the frame's unwired inputs. Authored
/// wires are kept, so the pass is idempotent.
pub(crate) fn wire_liquid_frame_cursor(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let mut added = Vec::new();
    for frame in def.nodes.iter().filter(|n| n.type_id == "node.liquid_frame") {
        let Some(clock) = def.wires.iter().find(|w| w.to_node == frame.id && w.to_port == "epoch").map(|w| w.from_node) else {
            continue;
        };
        if !def.nodes.iter().any(|n| n.id == clock && n.type_id == GPU_FLIP_DOMAIN_TYPE_ID) {
            continue;
        }
        for port in ["display_cursor", "dropped_seconds"] {
            if !def.wires.iter().any(|w| w.to_node == frame.id && w.to_port == port) {
                added.push(EffectGraphWire { from_node: clock, from_port: port.into(), to_node: frame.id, to_port: port.into() });
            }
        }
    }
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Move saved whitewater interpolators onto the frame's retained class copies
/// (GPU_FLIP_DISPLAY_HISTORY_DESIGN.md section 3.6 (Whitewater)), after
/// flattening. Matches only an interpolator whose `particles_b` is a
/// `node.liquid_state` class output and whose `blend` and `span` both come
/// from one `node.liquid_frame` reading that same state. The class wire goes
/// into the frame unless the frame's class input is already wired elsewhere,
/// which leaves the interpolator untouched; every other input is kept. A
/// migrated graph no longer matches, so the pass is idempotent.
pub(crate) fn wire_retained_whitewater(def: &mut EffectGraphDef) -> bool {
    use manifold_water_surface::primitives::liquid_frame::{WHITEWATER_INPUTS, WHITEWATER_OUTPUTS};
    use manifold_core::effect_graph_def::EffectGraphWire;
    const CLASSES: [&str; 4] = ["foam_particles", "bubble_particles", "spray_particles", "dust_particles"];
    let type_of = |def: &EffectGraphDef, id: u32| def.nodes.iter().find(|n| n.id == id).map(|n| n.type_id.clone());
    let feeding = |def: &EffectGraphDef, node: u32, port: &str| {
        def.wires.iter().find(|w| w.to_node == node && w.to_port == port).map(|w| (w.from_node, w.from_port.clone()))
    };
    let mut changed = false;
    let interpolators: Vec<u32> = def.nodes.iter()
        .filter(|n| n.type_id == "node.interpolate_particle_frames")
        .map(|n| n.id)
        .collect();
    for node in interpolators {
        let Some((state, class_port)) = feeding(def, node, "particles_b") else { continue };
        let Some(class) = CLASSES.iter().position(|c| *c == class_port) else { continue };
        if type_of(def, state).as_deref() != Some("node.liquid_state") {
            continue;
        }
        let (Some((frame, blend)), Some((span_frame, span))) = (feeding(def, node, "blend"), feeding(def, node, "span")) else { continue };
        if blend != "blend" || span != "span" || frame != span_frame || type_of(def, frame).as_deref() != Some("node.liquid_frame") {
            continue;
        }
        if feeding(def, frame, "particles").map(|(from, _)| from) != Some(state) {
            continue;
        }
        let class_in = WHITEWATER_INPUTS[class];
        match feeding(def, frame, class_in) {
            Some((from, port)) if from == state && port == class_port => {}
            Some(_) => continue,
            None => def.wires.push(EffectGraphWire { from_node: state, from_port: class_port.clone(), to_node: frame, to_port: class_in.into() }),
        }
        def.wires.retain(|w| !(w.to_node == node && w.to_port == "particles_b"));
        def.wires.push(EffectGraphWire { from_node: frame, from_port: WHITEWATER_OUTPUTS[class].into(), to_node: node, to_port: "particles_b".into() });
        changed = true;
    }
    changed
}

/// Correct standard GPU FLIP solid producers at the flattened installation seam.
/// Authored lattice wires retain their old meaning; only the exact native-grid
/// consumers move to the domain's mesh descriptor. Shared producers are cloned.
pub(crate) fn wire_gpu_flip_grid(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let mut changed = false;
    // Installation calls this after flatten_groups: ids and wires are local
    // to the same flat scope even when the saved graph contains nested groups.
    let geometry_matches = |node, domain, native: bool| {
        ["x", "y", "z"].into_iter().all(|axis| {
            [(format!("lattice_min_{axis}"), format!("mesh_min_{axis}")),
             (format!("nodes_{axis}"), format!("mesh_nodes_{axis}"))].into_iter().all(|(input, mesh)| {
                let output = if native { mesh.as_str() } else { input.as_str() };
                def.wires.iter().any(|w| w.to_node == node && w.to_port == input
                    && w.from_node == domain && w.from_port == output)
            })
        }) && def.wires.iter().any(|w| w.to_node == node && w.to_port == "cell_size"
            && w.from_node == domain && w.from_port == "cell_size")
    };
    let frames: Vec<_> = def.nodes.iter().filter(|n| n.type_id == "node.liquid_frame")
        .filter_map(|frame| {
            let domain = def.wires.iter().find(|w| w.to_node == frame.id && w.to_port == "nodes_x")?.from_node;
            (def.nodes.iter().any(|n| n.id == domain && n.type_id == GPU_FLIP_DOMAIN_TYPE_ID)
                && geometry_matches(frame.id, domain, false)).then_some((frame.id, domain))
        }).collect();
    // Capture matches before mutating wires so the geometric authority stays
    // immutable while a shared source is cloned and only FLIP uses move.
    let producers: Vec<_> = frames.iter().map(|&(frame, domain)| {
        let matches = def.nodes.iter().filter(|node| matches!(node.type_id.as_str(),
            "node.liquid_solid_distance" | "node.whitewater_obstacle_source"))
            .filter(|node| {
                let native = geometry_matches(node.id, domain, true);
                let authored = geometry_matches(node.id, domain, false);
                if !native && !authored { return false; }
                let inset_wire = def.wires.iter().find(|w| w.to_node == node.id && w.to_port == "wall_inset");
                if let Some(wire) = inset_wire {
                    return native && wire.from_node == domain && wire.from_port == "mesh_wall_inset";
                }
                // Only standard walls migrate. Authored/custom inset controls
                // must retain their meaning rather than becoming native walls.
                if node.exposed_params.contains("wall_inset") || def.preset_metadata.as_ref().is_some_and(|m|
                    m.bindings.iter().any(|b| matches!(&b.target,
                        manifold_core::effect_graph_def::BindingTarget::Node { node_id, param }
                            if *node_id == node.node_id && param == "wall_inset"))) { return false; }
                let inset = match node.params.get("wall_inset") {
                    Some(manifold_core::effect_graph_def::SerializedParamValue::Float { value }) => *value,
                    Some(manifold_core::effect_graph_def::SerializedParamValue::Int { value }) => *value as f32,
                    None => manifold_water_liquid::lattice::PADDING_NODES as f32,
                    _ => return false,
                };
                inset == if native { manifold_water_liquid::lattice::SURFACE_PADDING_CELLS }
                    else { manifold_water_liquid::lattice::PADDING_NODES as f32 }
            }).map(|n| (n.id, n.type_id.clone())).collect::<Vec<_>>();
        (frame, domain, matches)
    }).collect();
    let mut next_id = def.nodes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    for (frame, domain, matches) in producers {
        for (source, kind) in matches {
            let targets: Vec<_> = def.wires.iter().filter(|wire| wire.from_node == source && wire.from_port == "solid")
                .filter(|wire| if kind == "node.liquid_solid_distance" {
                    wire.to_node == frame && wire.to_port == "solid"
                } else {
                    wire.to_port == "obstacle_source" && def.nodes.iter().any(|node| node.id == wire.to_node
                        && node.type_id == "node.whitewater_step")
                        && def.wires.iter().any(|w| w.to_node == wire.to_node && w.to_port == "forces"
                            && w.from_node == domain && w.from_port == "forces")
                }).cloned().collect();
            if targets.is_empty() { continue; }
            let already_native = ["x", "y", "z"].into_iter().all(|axis| {
                [(format!("lattice_min_{axis}"), format!("mesh_min_{axis}")),
                 (format!("nodes_{axis}"), format!("mesh_nodes_{axis}"))].into_iter().all(|(input, output)|
                    def.wires.iter().any(|w| w.to_node == source && w.to_port == input
                        && w.from_node == domain && w.from_port == output))
            }) && def.wires.iter().any(|w| w.to_node == source && w.to_port == "wall_inset"
                && w.from_node == domain && w.from_port == "mesh_wall_inset");
            if already_native { continue; }
            let shared = def.wires.iter().any(|w| w.from_node == source && !targets.contains(w));
            let target_source = if shared {
                let mut copy = def.nodes.iter().find(|n| n.id == source).expect("matched source").clone();
                copy.id = next_id;
                let original_id = copy.node_id.clone();
                copy.node_id = manifold_core::NodeId::new(format!("{}/native-flip-grid-{next_id}", original_id.as_str()));
                copy.handle = copy.handle.map(|handle| format!("{handle}/native-flip-grid-{next_id}"));
                copy.exposed_params.remove("wall_inset");
                // Preserve card bindings as a fanout to the cloned producer.
                if let Some(metadata) = &mut def.preset_metadata {
                    use manifold_core::effect_graph_def::BindingTarget;
                    let bindings: Vec<_> = metadata.bindings.iter().filter_map(|binding| {
                        let BindingTarget::Node { node_id, param } = &binding.target else { return None };
                        if *node_id != original_id || param == "wall_inset" { return None; }
                        let mut binding = binding.clone();
                        binding.target = BindingTarget::Node { node_id: copy.node_id.clone(), param: param.clone() };
                        Some(binding)
                    }).collect();
                    metadata.bindings.extend(bindings);
                }
                def.nodes.push(copy);
                let inputs: Vec<_> = def.wires.iter().filter(|w| w.to_node == source).cloned().collect();
                for mut wire in inputs { wire.to_node = next_id; def.wires.push(wire); }
                for target in &targets {
                    let wire = def.wires.iter_mut().find(|w| *w == target).expect("target still present");
                    wire.from_node = next_id;
                }
                next_id += 1;
                next_id - 1
            } else { source };
            for axis in ["x", "y", "z"] {
                for (input, output) in [(format!("lattice_min_{axis}"), format!("mesh_min_{axis}")),
                                        (format!("nodes_{axis}"), format!("mesh_nodes_{axis}"))] {
                    let wire = def.wires.iter_mut().find(|w| w.to_node == target_source && w.to_port == input)
                        .expect("all descriptor wires matched");
                    wire.from_port = output;
                }
            }
            def.wires.retain(|w| !(w.to_node == target_source && w.to_port == "wall_inset"));
            def.wires.push(EffectGraphWire { from_node: domain, from_port: "mesh_wall_inset".into(),
                to_node: target_source, to_port: "wall_inset".into() });
            changed = true;
        }
        // Face component's nodes inputs have always meant authored-grid
        // counts. Restore that contract when an old graph used frame nodes.
        for node in &def.nodes {
            if node.type_id != "node.face_sample_component" { continue; }
            let from_frame = ["x", "y", "z"].into_iter().all(|axis| def.wires.iter().any(|w|
                w.to_node == node.id && w.to_port == format!("nodes_{axis}")
                    && w.from_node == frame && w.from_port == format!("grid_nodes_{axis}")));
            if !from_frame { continue; }
            for axis in ["x", "y", "z"] {
                if let Some(wire) = def.wires.iter_mut().find(|w| w.to_node == node.id && w.to_port == format!("nodes_{axis}")
                    && w.from_node == frame && w.from_port == format!("grid_nodes_{axis}")) {
                    wire.from_node = domain;
                    wire.from_port = format!("nodes_{axis}");
                    changed = true;
                }
            }
        }
    }
    // The engine seeds no site its walls hold; an old fill learns where they are.
    let fills: Vec<_> = def.nodes.iter().filter(|n| n.type_id == "node.liquid_fill").filter_map(|fill| {
        let domain = def.wires.iter().find(|w| w.to_node == fill.id && w.to_port == "nodes_x")?.from_node;
        (def.nodes.iter().any(|n| n.id == domain && n.type_id == GPU_FLIP_DOMAIN_TYPE_ID)
            && !def.wires.iter().any(|w| w.to_node == fill.id && w.to_port == "wall_inset"))
            .then_some((fill.id, domain))
    }).collect();
    for (fill, domain) in fills {
        def.wires.push(EffectGraphWire { from_node: domain, from_port: "mesh_wall_inset".into(),
            to_node: fill, to_port: "wall_inset".into() });
        changed = true;
    }
    changed
}


inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_liquid_intervals",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 300,
        apply: wire_liquid_intervals,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_gpu_flip_grid",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 310,
        apply: wire_gpu_flip_grid,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_gpu_flip_grid",
        stage: manifold_node_engine::load::migration::MigrationStage::BeforeBindingCapture,
        order: 310,
        apply: wire_gpu_flip_grid,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_liquid_frame_cursor",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 320,
        apply: wire_liquid_frame_cursor,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_retained_whitewater",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 330,
        apply: wire_retained_whitewater,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::EffectGraphNode;

    fn bare_node(id: u32, type_id: &str) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::default(),
            type_id: type_id.to_string(),
            handle: None,
            params: Default::default(),
            exposed_params: Default::default(),
            editor_pos: None,
            wgsl_source: None,
            title: None,
            output_formats: Default::default(),
            output_canvas_scales: Default::default(),
            group: None,
        }
    }

    /// A saved GPU FLIP whitewater wiring: state 1, frame 2, interpolators
    /// 3 (foam) and 4 (spray) sharing the frame.
    fn saved_whitewater() -> EffectGraphDef {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, "node.liquid_state"), bare_node(2, "node.liquid_frame"),
                bare_node(3, "node.interpolate_particle_frames"), bare_node(4, "node.interpolate_particle_frames")],
            wires: vec![
                wire(1, "out", 2, "particles"),
                wire(1, "foam_particles", 3, "particles_b"), wire(2, "blend", 3, "blend"), wire(2, "span", 3, "span"),
                wire(1, "spray_particles", 4, "particles_b"), wire(2, "blend", 4, "blend"), wire(2, "span", 4, "span"),
                wire(2, "count_b", 4, "count_b"),
            ],
        }
    }

    #[test]
    fn retained_whitewater_migration_is_exact_idempotent_and_survives_reload() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        // Shared frame: both classes move onto it, the authored count stays.
        let mut def = saved_whitewater();
        assert!(wire_retained_whitewater(&mut def));
        for (class, node) in [("foam", 3), ("spray", 4)] {
            assert!(def.wires.contains(&wire(1, &format!("{class}_particles"), 2, &format!("{class}_in"))));
            assert!(def.wires.contains(&wire(2, &format!("{class}_b"), node, "particles_b")));
            assert_eq!(def.wires.iter().filter(|w| w.to_node == node && w.to_port == "particles_b").count(), 1);
        }
        assert!(def.wires.contains(&wire(2, "count_b", 4, "count_b")));
        // Idempotent, and a save/reload stays migrated.
        let once = def.clone();
        assert!(!wire_retained_whitewater(&mut def));
        assert_eq!(def, once);
        let mut reloaded: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert!(!wire_retained_whitewater(&mut reloaded));
        assert_eq!(reloaded, once);

        // A conflicting explicit class input on the frame is preserved and
        // that interpolator left alone.
        let mut conflict = saved_whitewater();
        conflict.nodes.push(bare_node(5, "node.liquid_state"));
        conflict.wires.push(wire(5, "foam_particles", 2, "foam_in"));
        assert!(wire_retained_whitewater(&mut conflict));
        assert!(conflict.wires.contains(&wire(1, "foam_particles", 3, "particles_b")));
        assert!(conflict.wires.contains(&wire(5, "foam_particles", 2, "foam_in")));
        assert!(conflict.wires.contains(&wire(2, "spray_b", 4, "particles_b")));

        // Blend and span from different frames, or a frame reading another
        // state: no match.
        for custom in [
            { let mut d = saved_whitewater(); d.nodes.push(bare_node(6, "node.liquid_frame"));
              d.wires.retain(|w| !(w.to_node == 3 && w.to_port == "span")); d.wires.push(wire(6, "span", 3, "span"));
              d.wires.retain(|w| w.to_node != 4); d },
            { let mut d = saved_whitewater(); d.nodes.push(bare_node(7, "node.liquid_state"));
              d.wires.retain(|w| !(w.to_node == 2 && w.to_port == "particles")); d.wires.push(wire(7, "out", 2, "particles")); d },
        ] {
            let mut migrated = custom.clone();
            assert!(!wire_retained_whitewater(&mut migrated));
            assert_eq!(migrated, custom);
        }

        // Nested groups: the installation seam flattens first.
        let inner = saved_whitewater();
        let nested: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{ "id": 1, "typeId": "group", "handle": "outer", "group": {
                "interface": { "inputs": [], "outputs": [] },
                "nodes": [{ "id": 1, "typeId": "group", "handle": "inner", "group": {
                    "interface": { "inputs": [], "outputs": [] },
                    "nodes": inner.nodes, "wires": inner.wires
                }}], "wires": []
            }}], "wires": []
        })).unwrap();
        let mut flat = manifold_core::flatten::flatten_groups(&nested).unwrap();
        assert!(wire_retained_whitewater(&mut flat));
        assert_eq!(flat.wires.iter().filter(|w| w.to_port == "particles_b" && (w.from_port == "foam_b" || w.from_port == "spray_b")).count(), 2);
        assert!(!wire_retained_whitewater(&mut flat));
    }

    #[test]
    fn liquid_frame_cursor_migration_binds_the_frame_clock() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        // 1 GPU FLIP domain, 2 Matter domain, 3 frame. The Matter domain feeds
        // another input; the clock is the epoch's source.
        let base = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID), bare_node(2, MATTER_DOMAIN_TYPE_ID), bare_node(3, "node.liquid_frame")],
            wires: vec![wire(2, "closed_faces", 3, "closed_faces"), wire(1, "display_time", 3, "display_time"), wire(1, "epoch", 3, "epoch")],
        };
        let expected = [wire(1, "display_cursor", 3, "display_cursor"), wire(1, "dropped_seconds", 3, "dropped_seconds")];
        // Every order of the authored wires gives the same result.
        let orders = [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]];
        for order in orders {
            let mut def = base.clone();
            def.wires = order.iter().map(|&i| base.wires[i].clone()).collect();
            assert!(wire_liquid_frame_cursor(&mut def));
            for w in &expected {
                assert_eq!(def.wires.iter().filter(|x| *x == w).count(), 1, "{order:?}");
            }
            assert!(!def.wires.iter().any(|w| w.from_node == 2 && w.to_port == "display_cursor"));
            // Idempotent, and a save/reload stays migrated.
            let once = def.clone();
            assert!(!wire_liquid_frame_cursor(&mut def));
            let mut reloaded: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
            assert!(!wire_liquid_frame_cursor(&mut reloaded));
            assert_eq!(reloaded, once);
        }
        // A frame clocked by a Matter domain is untouched.
        let mut matter = base.clone();
        matter.wires[2] = wire(2, "epoch", 3, "epoch");
        let before = matter.clone();
        assert!(!wire_liquid_frame_cursor(&mut matter));
        assert_eq!(matter, before);
        // An authored constant 0 stays.
        let mut authored = base.clone();
        authored.nodes.push(bare_node(4, "node.constant"));
        authored.wires.push(wire(4, "value", 3, "display_cursor"));
        assert!(wire_liquid_frame_cursor(&mut authored));
        assert_eq!(authored.wires.iter().filter(|w| w.to_port == "display_cursor").count(), 1);
        assert!(authored.wires.contains(&wire(4, "value", 3, "display_cursor")));
        // Nested groups: the installation seam flattens first.
        let nested: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{ "id": 1, "typeId": "group", "handle": "outer", "group": {
                "interface": { "inputs": [], "outputs": [] },
                "nodes": base.nodes, "wires": base.wires
            }}], "wires": []
        })).unwrap();
        let mut flat = manifold_core::flatten::flatten_groups(&nested).unwrap();
        assert!(wire_liquid_frame_cursor(&mut flat));
        assert_eq!(flat.wires.iter().filter(|w| w.to_port == "display_cursor").count(), 1);
        // A saved project layer from before the cursor gets it from its domain.
        let mut saved: EffectGraphDef = serde_json::from_str(include_str!(
            "../../../manifold-io/tests/fixtures/water_layer_graph_v1160.json"
        ))
        .expect("saved layer");
        saved.scene_modifiers.clear();
        let mut saved = manifold_core::flatten::flatten_groups(&saved).expect("flattens");
        assert!(wire_liquid_frame_cursor(&mut saved));
        let frame = saved.nodes.iter().find(|n| n.type_id == "node.liquid_frame").expect("a frame").id;
        let feed = saved.wires.iter().find(|w| w.to_node == frame && w.to_port == "display_cursor").expect("wired");
        assert!(saved.nodes.iter().any(|n| n.id == feed.from_node && n.type_id == GPU_FLIP_DOMAIN_TYPE_ID));
    }

    #[test]
    fn native_flip_grid_migration_clones_shared_solids_and_preserves_authored_wires() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID), bare_node(2, "node.liquid_solid_distance"),
                bare_node(3, "node.liquid_frame"), bare_node(4, "node.value_sink"), bare_node(5, "node.liquid_fill")],
            wires: vec![wire(2, "solid", 3, "solid"), wire(2, "solid", 4, "in"), wire(1, "cell_size", 3, "cell_size"), wire(1, "cell_size", 2, "cell_size")],
        };
        for axis in ["x", "y", "z"] {
            for port in [format!("lattice_min_{axis}"), format!("nodes_{axis}")] {
                def.wires.push(wire(1, &port, 2, &port));
                def.wires.push(wire(1, &port, 5, &port));
                def.wires.push(wire(1, &port, 3, &port));
            }
        }
        // Numeric ids repeat across two group scopes. Installation flattens
        // first, so the unrelated outer domain cannot capture inner wires.
        let nested: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [
                { "id": 1, "typeId": GPU_FLIP_DOMAIN_TYPE_ID },
                { "id": 2, "typeId": "group", "handle": "outer", "group": {
                    "interface": { "inputs": [], "outputs": [] },
                    "nodes": [{ "id": 1, "typeId": "group", "handle": "inner", "group": {
                        "interface": { "inputs": [], "outputs": [] },
                        "nodes": def.nodes.clone(), "wires": def.wires.clone()
                    }}], "wires": []
                }}
            ], "wires": []
        })).unwrap();
        let mut flat = manifold_core::flatten::flatten_groups(&nested).unwrap();
        assert!(wire_gpu_flip_grid(&mut flat));
        assert_eq!(flat.nodes.iter().filter(|n| n.type_id == "node.liquid_solid_distance").count(), 2);
        assert!(!wire_gpu_flip_grid(&mut flat));

        let authored = def.wires.clone();
        assert!(wire_gpu_flip_grid(&mut def));
        let clone = def.wires.iter().find(|w| w.to_node == 3 && w.to_port == "solid").unwrap().from_node;
        assert_ne!(clone, 2);
        assert!(def.wires.contains(&wire(2, "solid", 4, "in")));
        assert!(def.wires.contains(&wire(1, "mesh_wall_inset", clone, "wall_inset")));
        assert!(def.wires.contains(&wire(1, "mesh_wall_inset", 5, "wall_inset")));
        for axis in ["x", "y", "z"] {
            assert!(def.wires.contains(&wire(1, &format!("mesh_min_{axis}"), clone, &format!("lattice_min_{axis}"))));
            assert!(def.wires.contains(&wire(1, &format!("mesh_nodes_{axis}"), clone, &format!("nodes_{axis}"))));
        }
        for wire in authored.into_iter().filter(|w| w.to_node == 2 || w.to_node == 5) {
            assert!(def.wires.contains(&wire));
        }
        assert!(!wire_gpu_flip_grid(&mut def));
    }

    #[test]
    fn native_flip_grid_migration_preserves_mixed_geometry_and_custom_walls() {
        use manifold_core::effect_graph_def::{EffectGraphWire, SerializedParamValue};
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID), bare_node(2, "node.liquid_solid_distance"),
                bare_node(3, "node.liquid_frame"), bare_node(4, GPU_FLIP_DOMAIN_TYPE_ID)],
            wires: vec![EffectGraphWire { from_node: 2, from_port: "solid".into(), to_node: 3, to_port: "solid".into() }],
        };
        for node in [2, 3] {
            for port in ["lattice_min_x", "lattice_min_y", "lattice_min_z", "nodes_x", "nodes_y", "nodes_z", "cell_size"] {
                def.wires.push(EffectGraphWire { from_node: 1, from_port: port.into(), to_node: node, to_port: port.into() });
            }
        }
        for (node, port) in [(2, "cell_size"), (3, "cell_size"), (3, "nodes_y"), (3, "lattice_min_z")] {
            let mut custom = def.clone();
            custom.wires.iter_mut().find(|w| w.to_node == node && w.to_port == port).unwrap().from_node = 4;
            let before = custom.clone();
            assert!(!wire_gpu_flip_grid(&mut custom), "custom {node}.{port}");
            assert_eq!(custom, before);
        }
        def.nodes[1].params.insert("wall_inset".into(), SerializedParamValue::Float { value: 2.0 });
        let before = def.clone();
        assert!(!wire_gpu_flip_grid(&mut def));
        assert_eq!(def, before);
    }

    #[test]
    fn native_flip_grid_migration_ignores_custom_and_matter_producers() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, MATTER_DOMAIN_TYPE_ID), bare_node(2, "node.liquid_solid_distance"), bare_node(3, "node.liquid_frame")],
            wires: vec![EffectGraphWire { from_node: 1, from_port: "nodes_x".into(), to_node: 3, to_port: "nodes_x".into() }],
        };
        let original = def.clone();
        assert!(!wire_gpu_flip_grid(&mut def));
        assert_eq!(def, original);
        def.nodes[0].type_id = GPU_FLIP_DOMAIN_TYPE_ID.into();
        def.nodes[1].type_id = "node.custom_solid".into();
        let original = def.clone();
        assert!(!wire_gpu_flip_grid(&mut def));
        assert_eq!(def, original);
    }

    #[test]
    fn live_clock_graph_wires_are_explicit_idempotent_and_preserve_authored_duration() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID), bare_node(2, "node.liquid_state"), bare_node(3, "node.gpu_flip_step"), bare_node(4, "node.whitewater_step"), bare_node(5, "node.scalar")],
            wires: vec![wire(1, "ticks", 2, "ticks"), wire(2, "out", 3, "particles"), wire(3, "out", 2, "in"), wire(3, "faces", 4, "faces"), wire(5, "out", 4, "dt")],
        };
        assert!(wire_liquid_intervals(&mut def));
        assert!(def.wires.contains(&wire(1, "interval_duration", 3, "interval_duration")));
        assert!(def.wires.contains(&wire(1, "live_hits", 3, "live_hits")));
        assert!(def.wires.contains(&wire(1, "initial_obstacle_speed", 3, "initial_obstacle_speed")));
        assert!(def.wires.contains(&wire(3, "clock_status", 2, "clock_status_in")));
        assert!(def.wires.contains(&wire(2, "identity", 3, "identity")));
        assert!(def.wires.contains(&wire(2, "retired_max_speed", 3, "retired_max_speed")));
        assert!(def.wires.contains(&wire(3, "identity_out", 2, "identity_in")));
        assert_eq!(def.wires.iter().filter(|w| w.to_node == 4 && w.to_port == "dt").count(), 1);
        assert!(def.wires.contains(&wire(5, "out", 4, "dt")));
        assert!(!wire_liquid_intervals(&mut def));
    }

    #[test]
    fn dropped_time_wires_are_added_once_and_preserve_authored_inputs() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        for (domain, state) in [
            (GPU_FLIP_DOMAIN_TYPE_ID, "node.liquid_state"),
            (MATTER_DOMAIN_TYPE_ID, "node.matter_state"),
        ] {
            let authored = wire(4, "out", 3, "dropped_seconds");
            let mut def = EffectGraphDef {
                version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
                name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
                nodes: vec![bare_node(1, domain), bare_node(2, state), bare_node(3, state), bare_node(4, "node.scalar"), bare_node(5, state)],
                wires: vec![wire(1, "ticks", 2, "ticks"), wire(1, "ticks", 3, "ticks"), authored.clone()],
            };
            assert!(wire_liquid_intervals(&mut def));
            assert!(def.wires.contains(&wire(1, "dropped_seconds", 2, "dropped_seconds")));
            assert!(def.wires.contains(&authored));
            for state in [2, 3] {
                assert_eq!(def.wires.iter().filter(|w| w.to_node == state && w.to_port == "dropped_seconds").count(), 1);
            }
            assert!(!def.wires.iter().any(|w| w.to_node == 5), "an unowned state has no inferred clock");
            let migrated = def.wires.clone();
            assert!(!wire_liquid_intervals(&mut def));
            assert_eq!(def.wires, migrated);
        }
    }

    #[test]
    fn retired_speed_wires_preserve_authored_inputs_and_follow_each_steps_state() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        let authored = wire(5, "out", 4, "retired_max_speed");
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
            nodes: vec![bare_node(1, "node.liquid_state"), bare_node(2, "node.gpu_flip_step"), bare_node(3, "node.liquid_state"), bare_node(4, "node.gpu_flip_step"), bare_node(5, "node.scalar"), bare_node(6, "node.gpu_flip_step")],
            wires: vec![wire(1, "out", 2, "particles"), wire(3, "out", 4, "particles"), authored.clone()],
        };
        assert!(wire_liquid_intervals(&mut def));
        assert!(def.wires.contains(&wire(1, "retired_max_speed", 2, "retired_max_speed")));
        assert!(def.wires.contains(&authored));
        for step in [2, 4] {
            assert_eq!(def.wires.iter().filter(|w| w.to_node == step && w.to_port == "retired_max_speed").count(), 1);
        }
        assert!(!def.wires.iter().any(|w| w.to_node == 6), "no state is inferred for an unconnected step");
        let migrated = def.wires.clone();
        assert!(!wire_liquid_intervals(&mut def));
        assert_eq!(def.wires, migrated);
    }

    #[test]
    fn whitewater_obstacle_source_uses_accepted_duration_without_overwriting_wires() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from, from_port: output.into(), to_node: to, to_port: input.into(),
        };
        for domain in [GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID] {
            let mut def = EffectGraphDef {
                version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
                name: None, description: None, preset_metadata: None, scene_modifiers: Vec::new(),
                nodes: vec![bare_node(1, domain), bare_node(2, "node.whitewater_obstacle_source"), bare_node(3, "node.whitewater_obstacle_source"), bare_node(4, "node.scalar")],
                wires: vec![wire(1, "bodies", 2, "bodies"), wire(1, "bodies", 3, "bodies"), wire(4, "out", 3, "tick_seconds")],
            };
            assert!(wire_liquid_intervals(&mut def));
            assert!(def.wires.contains(&wire(1, "interval_duration", 2, "tick_seconds")));
            assert!(def.wires.contains(&wire(4, "out", 3, "tick_seconds")));
            assert_eq!(def.wires.iter().filter(|w| w.to_node == 3 && w.to_port == "tick_seconds").count(), 1);
            assert!(!wire_liquid_intervals(&mut def));
        }
    }

}
