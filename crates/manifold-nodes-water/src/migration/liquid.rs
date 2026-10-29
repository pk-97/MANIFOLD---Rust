//! Load migrations for liquid clocks, frame cursors and retained solver outputs.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::liquid_domain::{GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID};

/// Add explicit interval and clock telemetry wires to pre-clock liquid graphs,
/// preserving authored wires and all export settings. Runs once at installation.
fn wire_liquid_intervals(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let mut domains = std::collections::BTreeMap::new();
    for node in &def.nodes {
        if matches!(
            node.type_id.as_str(),
            GPU_FLIP_DOMAIN_TYPE_ID | MATTER_DOMAIN_TYPE_ID
        ) {
            domains.insert(node.id, node.id);
        }
    }
    // Follow only solver boundaries, never arbitrary graph ancestry.
    for _ in 0..3 {
        for node in &def.nodes {
            if !matches!(
                node.type_id.as_str(),
                "node.liquid_state" | "node.matter_state" | "node.gpu_flip_step"
            ) {
                continue;
            }
            let source = def
                .wires
                .iter()
                .filter(|wire| wire.to_node == node.id)
                .filter_map(|wire| domains.get(&wire.from_node).copied())
                .next();
            if let Some(source) = source {
                domains.insert(node.id, source);
            }
        }
    }
    let mut changed = false;
    for node in &def.nodes {
        let clock_ports: &[(&str, &str)] = match node.type_id.as_str() {
            "node.gpu_flip_step" => &[
                ("clock_obstacles", "clock_obstacles"),
                ("clock_sources", "clock_sources"),
                ("clock_obstacle_count", "clock_obstacle_count"),
                ("clock_source_count", "clock_source_count"),
                ("live_hits", "live_hits"),
                ("live_hit_count", "live_hit_count"),
                ("limit_interval", "limit_interval"),
            ],
            "node.liquid_state" => &[("dropped_seconds", "dropped_seconds")],
            "node.matter_state" => &[
                ("target_time", "target_time"),
                ("simulation_time", "simulation_time"),
                ("step_cap_hit", "step_cap_hit"),
                ("dropped_seconds", "dropped_seconds"),
            ],
            _ => &[],
        };
        let intervals = manifold_water_liquid::clock::INTERVAL_DURATION_INPUTS
            .iter()
            .filter(|(type_id, _)| *type_id == node.type_id.as_str())
            .map(|&(_, input)| ("interval_duration", input));
        let source = def
            .wires
            .iter()
            .filter(|wire| wire.to_node == node.id)
            .find_map(|wire| domains.get(&wire.from_node).copied());
        if let Some(source) = source {
            for (output, input) in intervals.chain(clock_ports.iter().copied()) {
                if def
                    .wires
                    .iter()
                    .any(|wire| wire.to_node == node.id && wire.to_port == input)
                {
                    continue;
                }
                def.wires.push(EffectGraphWire {
                    from_node: source,
                    from_port: output.into(),
                    to_node: node.id,
                    to_port: input.into(),
                });
                changed = true;
            }
        }
        if matches!(
            node.type_id.as_str(),
            "node.gpu_flip_step" | "node.liquid_frame"
        ) {
            let state = def
                .wires
                .iter()
                .filter(|w| w.to_node == node.id && w.to_port == "particles")
                .find_map(|w| {
                    def.nodes
                        .iter()
                        .find(|n| n.id == w.from_node && n.type_id == "node.liquid_state")
                });
            if let Some(state) = state {
                if !def
                    .wires
                    .iter()
                    .any(|w| w.to_node == node.id && w.to_port == "identity")
                {
                    def.wires.push(EffectGraphWire {
                        from_node: state.id,
                        from_port: "identity".into(),
                        to_node: node.id,
                        to_port: "identity".into(),
                    });
                    changed = true;
                }
                if node.type_id == "node.gpu_flip_step"
                    && !def
                        .wires
                        .iter()
                        .any(|w| w.to_node == state.id && w.to_port == "identity_in")
                {
                    def.wires.push(EffectGraphWire {
                        from_node: node.id,
                        from_port: "identity_out".into(),
                        to_node: state.id,
                        to_port: "identity_in".into(),
                    });
                    changed = true;
                }
            }
        }
        if node.type_id == "node.liquid_stats"
            && !def
                .wires
                .iter()
                .any(|w| w.to_node == node.id && w.to_port == "identity")
        {
            // The identity the measured step hands on counts the slots in use.
            let step = def
                .wires
                .iter()
                .filter(|w| w.to_node == node.id && w.to_port == "particles")
                .find_map(|w| {
                    def.nodes
                        .iter()
                        .find(|n| n.id == w.from_node && n.type_id == "node.gpu_flip_step")
                });
            if let Some(step) = step {
                def.wires.push(EffectGraphWire {
                    from_node: step.id,
                    from_port: "identity_out".into(),
                    to_node: node.id,
                    to_port: "identity".into(),
                });
                changed = true;
            }
        }
        if node.type_id == "node.liquid_state"
            && !def
                .wires
                .iter()
                .any(|w| w.to_node == node.id && w.to_port == "clock_status_in")
        {
            let step = def
                .wires
                .iter()
                .filter(|w| w.to_node == node.id && w.to_port == "in")
                .find_map(|w| {
                    def.nodes
                        .iter()
                        .find(|n| n.id == w.from_node && n.type_id == "node.gpu_flip_step")
                });
            if let Some(step) = step {
                def.wires.push(EffectGraphWire {
                    from_node: step.id,
                    from_port: "clock_status".into(),
                    to_node: node.id,
                    to_port: "clock_status_in".into(),
                });
                changed = true;
            }
        }
    }
    changed
}

/// Give saved GPU FLIP frames the presentation cursor
/// (the Cursor), after
/// flattening. The frame's clock is the node feeding its `epoch`, never the
/// first incoming wire; only a GPU FLIP domain there gets `display_cursor`
/// and `dropped_seconds` wired into the frame's unwired inputs. Authored
/// wires are kept, so the pass is idempotent.
pub(crate) fn wire_liquid_frame_cursor(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let mut added = Vec::new();
    for frame in def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.liquid_frame")
    {
        let Some(clock) = def
            .wires
            .iter()
            .find(|w| w.to_node == frame.id && w.to_port == "epoch")
            .map(|w| w.from_node)
        else {
            continue;
        };
        if !def
            .nodes
            .iter()
            .any(|n| n.id == clock && n.type_id == GPU_FLIP_DOMAIN_TYPE_ID)
        {
            continue;
        }
        for port in ["display_cursor", "dropped_seconds"] {
            if !def
                .wires
                .iter()
                .any(|w| w.to_node == frame.id && w.to_port == port)
            {
                added.push(EffectGraphWire {
                    from_node: clock,
                    from_port: port.into(),
                    to_node: frame.id,
                    to_port: port.into(),
                });
            }
        }
    }
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Move saved whitewater interpolators onto the frame's retained class copies
/// (the Whitewater), after
/// flattening. Matches only an interpolator whose `particles_b` is a
/// `node.liquid_state` class output and whose `blend` and `span` both come
/// from one `node.liquid_frame` reading that same state. The class wire goes
/// into the frame unless the frame's class input is already wired elsewhere,
/// which leaves the interpolator untouched; every other input is kept. A
/// migrated graph no longer matches, so the pass is idempotent.
pub(crate) fn wire_retained_whitewater(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    use manifold_water_surface::primitives::liquid_frame::{WHITEWATER_INPUTS, WHITEWATER_OUTPUTS};
    const CLASSES: [&str; 4] = [
        "foam_particles",
        "bubble_particles",
        "spray_particles",
        "dust_particles",
    ];
    let type_of = |def: &EffectGraphDef, id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.clone())
    };
    let feeding = |def: &EffectGraphDef, node: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == port)
            .map(|w| (w.from_node, w.from_port.clone()))
    };
    let mut changed = false;
    let interpolators: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.interpolate_particle_frames")
        .map(|n| n.id)
        .collect();
    for node in interpolators {
        let Some((state, class_port)) = feeding(def, node, "particles_b") else {
            continue;
        };
        let Some(class) = CLASSES.iter().position(|c| *c == class_port) else {
            continue;
        };
        if type_of(def, state).as_deref() != Some("node.liquid_state") {
            continue;
        }
        let (Some((frame, blend)), Some((span_frame, span))) =
            (feeding(def, node, "blend"), feeding(def, node, "span"))
        else {
            continue;
        };
        if blend != "blend"
            || span != "span"
            || frame != span_frame
            || type_of(def, frame).as_deref() != Some("node.liquid_frame")
        {
            continue;
        }
        if feeding(def, frame, "particles").map(|(from, _)| from) != Some(state) {
            continue;
        }
        let class_in = WHITEWATER_INPUTS[class];
        match feeding(def, frame, class_in) {
            Some((from, port)) if from == state && port == class_port => {}
            Some(_) => continue,
            None => def.wires.push(EffectGraphWire {
                from_node: state,
                from_port: class_port.clone(),
                to_node: frame,
                to_port: class_in.into(),
            }),
        }
        def.wires
            .retain(|w| !(w.to_node == node && w.to_port == "particles_b"));
        def.wires.push(EffectGraphWire {
            from_node: frame,
            from_port: WHITEWATER_OUTPUTS[class].into(),
            to_node: node,
            to_port: "particles_b".into(),
        });
        changed = true;
    }
    changed
}

/// Move saved GPU FLIP graphs onto the step's pooled outputs, after
/// flattening. The liquid state's dense face copy (`faces_in`, `faces`) and
/// the frame's face grid are gone: the wire into `faces_in`, the face
/// components fed by the state's `faces` with every wire they carry, and the
/// frame's face wires are dropped. A whitewater step or face component reading
/// the step's pooled `faces` or `distance` gains the step's `tiles` and
/// `pool_tiles` beside them. Idempotent.
pub(crate) fn retire_dense_liquid_faces(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    const FRAME_FACE_PORTS: [&str; 10] = [
        "face_u_in",
        "face_v_in",
        "face_w_in",
        "face_u",
        "face_v",
        "face_w",
        "face_cells_x",
        "face_cells_y",
        "face_cells_z",
        "face_valid_layers",
    ];
    fn type_of(def: &EffectGraphDef, id: u32) -> Option<&str> {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.as_str())
    }
    let mut changed = false;
    let state_faces: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.face_sample_component")
        .filter(|n| {
            def.wires.iter().any(|w| {
                w.to_node == n.id
                    && w.to_port == "faces"
                    && w.from_port == "faces"
                    && type_of(def, w.from_node) == Some("node.liquid_state")
            })
        })
        .map(|n| n.id)
        .collect();
    let wires = def.wires.len();
    let nodes = def.nodes.len();
    def.nodes.retain(|n| !state_faces.contains(&n.id));
    let kinds: std::collections::HashMap<u32, String> = def
        .nodes
        .iter()
        .map(|n| (n.id, n.type_id.clone()))
        .collect();
    def.wires.retain(|w| {
        let kind = |id: u32| kinds.get(&id).map(String::as_str);
        let into_state = kind(w.to_node) == Some("node.liquid_state") && w.to_port == "faces_in";
        let out_of_state = kind(w.from_node) == Some("node.liquid_state") && w.from_port == "faces";
        let frame_face = (kind(w.to_node) == Some("node.liquid_frame")
            && FRAME_FACE_PORTS.contains(&w.to_port.as_str()))
            || (kind(w.from_node) == Some("node.liquid_frame")
                && FRAME_FACE_PORTS.contains(&w.from_port.as_str()));
        !(state_faces.contains(&w.from_node)
            || state_faces.contains(&w.to_node)
            || into_state
            || out_of_state
            || frame_face)
    });
    changed |= def.wires.len() != wires || def.nodes.len() != nodes;

    // Readers of the step's pooled buffers: the whitewater step (distance and
    // faces) and a face component fed by the step's faces.
    let mut added = Vec::new();
    for node in def.nodes.iter().filter(|n| {
        matches!(
            n.type_id.as_str(),
            "node.whitewater_step" | "node.face_sample_component"
        )
    }) {
        let step = def
            .wires
            .iter()
            .filter(|w| w.to_node == node.id && matches!(w.to_port.as_str(), "faces" | "distance"))
            .find(|w| {
                w.from_port == w.to_port && type_of(def, w.from_node) == Some("node.gpu_flip_step")
            })
            .map(|w| w.from_node);
        let Some(step) = step else { continue };
        // Whitewater also counts its emitters from the step's identity record.
        let ports: &[(&str, &str)] = if node.type_id == "node.whitewater_step" {
            &[
                ("tiles", "tiles"),
                ("pool_tiles", "pool_tiles"),
                ("identity_out", "identity"),
            ]
        } else {
            &[("tiles", "tiles"), ("pool_tiles", "pool_tiles")]
        };
        for &(output, input) in ports {
            if !def
                .wires
                .iter()
                .any(|w| w.to_node == node.id && w.to_port == input)
            {
                added.push(EffectGraphWire {
                    from_node: step,
                    from_port: output.into(),
                    to_node: node.id,
                    to_port: input.into(),
                });
            }
        }
    }
    changed |= !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Move saved GPU FLIP whitewater onto the step's world tiles, after
/// flattening. The whitewater step's dense grid and face ports (`grid_bounds`,
/// `grid_nodes_*`, `face_cells_*`, `face_valid_layers`, `face_u/v/w`) and its
/// per-axis history (`substep_u/v/w`) are gone: their wires drop, and a
/// whitewater fed the step's substep schedule reads its `substep_faces`. The
/// obstacle source drops its native-grid wires (`lattice_min_*`, `nodes_*`)
/// and reads the step's `tiles` and `pool_tiles`; its `distance` feeds the
/// whitewater's `solid` and its `source` the `obstacle_source`, in place of
/// the native solid lattice and the old `solid` output. The domain feeds the
/// whitewater its `cell_size` and `bounds_lo_x/y/z`. Idempotent.
pub(crate) fn pool_whitewater_on_step_tiles(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    const RETIRED_INPUTS: [&str; 14] = [
        "grid_bounds",
        "grid_nodes_x",
        "grid_nodes_y",
        "grid_nodes_z",
        "face_cells_x",
        "face_cells_y",
        "face_cells_z",
        "face_valid_layers",
        "face_u",
        "face_v",
        "face_w",
        "substep_u",
        "substep_v",
        "substep_w",
    ];
    const SOURCE_GRID: [&str; 6] = [
        "lattice_min_x",
        "lattice_min_y",
        "lattice_min_z",
        "nodes_x",
        "nodes_y",
        "nodes_z",
    ];
    const SOURCE: &str = "node.whitewater_obstacle_source";
    let kinds: std::collections::HashMap<u32, String> = def
        .nodes
        .iter()
        .map(|n| (n.id, n.type_id.clone()))
        .collect();
    let kind = |id: u32| kinds.get(&id).map(String::as_str);
    let feeder = |to: u32, ports: &[&str], from_kind: &str| {
        def.wires
            .iter()
            .find(|w| {
                w.to_node == to
                    && ports.contains(&w.to_port.as_str())
                    && kind(w.from_node) == Some(from_kind)
            })
            .map(|w| w.from_node)
    };
    let mut added: Vec<EffectGraphWire> = Vec::new();
    let mut wire = |from_node: u32, from_port: &str, to_node: u32, to_port: &str| {
        added.push(EffectGraphWire {
            from_node,
            from_port: from_port.into(),
            to_node,
            to_port: to_port.into(),
        });
    };
    for ww in def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.whitewater_step")
        .map(|n| n.id)
    {
        let Some(step) = feeder(ww, &["faces", "distance"], "node.gpu_flip_step") else {
            continue;
        };
        if def
            .wires
            .iter()
            .any(|w| w.to_node == ww && w.to_port == "substep_schedule" && w.from_node == step)
        {
            wire(step, "substep_faces", ww, "substep_faces");
        }
        if let Some(source) = feeder(ww, &["obstacle_source"], SOURCE) {
            wire(source, "distance", ww, "solid");
            wire(source, "source", ww, "obstacle_source");
            wire(step, "tiles", source, "tiles");
            wire(step, "pool_tiles", source, "pool_tiles");
        }
        let domain = def
            .wires
            .iter()
            .find(|w| w.to_node == step && kind(w.from_node) == Some(GPU_FLIP_DOMAIN_TYPE_ID))
            .map(|w| w.from_node);
        if let Some(domain) = domain {
            for port in ["cell_size", "bounds_lo_x", "bounds_lo_y", "bounds_lo_z"] {
                wire(domain, port, ww, port);
            }
        }
    }
    let before = def.wires.clone();
    def.wires.retain(|w| {
        let into_whitewater = kind(w.to_node) == Some("node.whitewater_step");
        let retired = into_whitewater && RETIRED_INPUTS.contains(&w.to_port.as_str());
        // Replaced below by the obstacle source's pooled outputs.
        let replaced = into_whitewater
            && matches!(w.to_port.as_str(), "solid" | "obstacle_source")
            && added
                .iter()
                .any(|a| a.to_node == w.to_node && a.to_port == w.to_port && a != w);
        let source_grid =
            kind(w.to_node) == Some(SOURCE) && SOURCE_GRID.contains(&w.to_port.as_str());
        let source_solid = kind(w.from_node) == Some(SOURCE) && w.from_port == "solid";
        !(retired || replaced || source_grid || source_solid)
    });
    for a in added {
        if !def
            .wires
            .iter()
            .any(|w| w.to_node == a.to_node && w.to_port == a.to_port)
        {
            def.wires.push(a);
        }
    }
    def.wires != before
}

/// The domain ports a whitewater step reads to remove whitewater where the
/// liquid step removes water: past an Open face, or below the kill height.
const WHITEWATER_REMOVAL: [&str; 5] = [
    "bounds_hi_x",
    "bounds_hi_y",
    "bounds_hi_z",
    "open_faces",
    "kill_height",
];

/// Wire a saved GPU FLIP whitewater step to the domain's Open faces, Bounds
/// high corner and kill height, after flattening, from the domain that feeds
/// its `bounds_lo_x`. A port already wired is kept. Idempotent.
pub(crate) fn wire_whitewater_removal(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let kind = |id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.as_str())
    };
    let mut added = Vec::new();
    for ww in def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.whitewater_step")
        .map(|n| n.id)
    {
        let Some(domain) = def
            .wires
            .iter()
            .find(|w| {
                w.to_node == ww
                    && w.to_port == "bounds_lo_x"
                    && kind(w.from_node) == Some(GPU_FLIP_DOMAIN_TYPE_ID)
            })
            .map(|w| w.from_node)
        else {
            continue;
        };
        for port in WHITEWATER_REMOVAL {
            if !def
                .wires
                .iter()
                .any(|w| w.to_node == ww && w.to_port == port)
            {
                added.push(EffectGraphWire {
                    from_node: domain,
                    from_port: port.into(),
                    to_node: ww,
                    to_port: port.into(),
                });
            }
        }
    }
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Wire a saved GPU FLIP fill to the domain's regions and shaped fill
/// region, after flattening, from the domain that feeds its `pool_sites`, so
/// a Fill shaped as a sphere or a model seeds inside its shape. A port
/// already wired is kept. Idempotent.
pub(crate) fn wire_shaped_fill(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let kind = |id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.as_str())
    };
    let mut added = Vec::new();
    for fill in def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.liquid_fill")
        .map(|n| n.id)
    {
        let Some(domain) = def
            .wires
            .iter()
            .find(|w| {
                w.to_node == fill
                    && w.to_port == "pool_sites"
                    && kind(w.from_node) == Some(GPU_FLIP_DOMAIN_TYPE_ID)
            })
            .map(|w| w.from_node)
        else {
            continue;
        };
        for port in crate::presets::gpu_flip::FILL_SHAPE_WIRES {
            if !def
                .wires
                .iter()
                .any(|w| w.to_node == fill && w.to_port == port)
            {
                added.push(EffectGraphWire {
                    from_node: domain,
                    from_port: port.into(),
                    to_node: fill,
                    to_port: port.into(),
                });
            }
        }
    }
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Drop a saved surface's `node.redistance_lattice`, after flattening: every
/// wire it fed takes its `levelset` source instead, the particles' splat
/// (`node.particle_volume` keeps a solid's nodes at their depth in it, which
/// is all the smoothing and the wetness needed from it). Idempotent.
pub(crate) fn retire_redistance(def: &mut EffectGraphDef) -> bool {
    const RETIRED: &str = "node.redistance_lattice";
    let retired: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == RETIRED)
        .map(|n| n.id)
        .collect();
    for &id in &retired {
        let source = def
            .wires
            .iter()
            .find(|w| w.to_node == id && w.to_port == "levelset")
            .map(|w| (w.from_node, w.from_port.clone()));
        def.wires.retain(|w| w.to_node != id);
        match source {
            Some((node, port)) => {
                for w in def.wires.iter_mut().filter(|w| w.from_node == id) {
                    w.from_node = node;
                    w.from_port.clone_from(&port);
                }
            }
            None => def.wires.retain(|w| w.from_node != id),
        }
    }
    def.nodes.retain(|n| n.type_id != RETIRED);
    !retired.is_empty()
}

/// Drop a saved water's `node.foam_cover` and the f32 switch it fed, after
/// flattening: what the switch fed (the water object's `field_foam`) takes
/// the cover's `particles` source instead, the foam particles the renderer
/// now draws at pixel resolution. A binding on the cover's strength moves to
/// that object's `foam_coverage`, and one on the switch goes with it.
/// Idempotent.
pub(crate) fn retire_foam_cover(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::BindingTarget;
    const RETIRED: &str = "node.foam_cover";
    let retired: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == RETIRED)
        .map(|n| n.id)
        .collect();
    let node_id = |def: &EffectGraphDef, id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.node_id.clone())
    };
    let mut gone = retired.clone();
    // (the cover's node id, the object its foam reached).
    let mut moved = Vec::new();
    for &id in &retired {
        let source = def
            .wires
            .iter()
            .find(|w| w.to_node == id && w.to_port == "particles")
            .map(|w| (w.from_node, w.from_port.clone()));
        // The switches the cover fed go too: they switch f32 arrays.
        let switches: Vec<u32> = def
            .wires
            .iter()
            .filter(|w| w.from_node == id)
            .map(|w| w.to_node)
            .filter(|&to| {
                def.nodes
                    .iter()
                    .any(|n| n.id == to && n.type_id == "node.switch_array")
            })
            .collect();
        if let (Some(cover), Some(object)) = (
            node_id(def, id),
            def.wires
                .iter()
                .find(|w| {
                    (w.from_node == id || switches.contains(&w.from_node))
                        && w.to_port == "field_foam"
                })
                .and_then(|w| node_id(def, w.to_node)),
        ) {
            moved.push((cover, object));
        }
        def.wires
            .retain(|w| w.to_node != id && !switches.contains(&w.to_node));
        for w in def.wires.iter_mut() {
            if w.from_node == id || switches.contains(&w.from_node) {
                match &source {
                    Some((node, port)) => {
                        w.from_node = *node;
                        w.from_port.clone_from(port);
                    }
                    None => w.from_node = id,
                }
            }
        }
        def.wires.retain(|w| w.from_node != id);
        gone.extend(switches);
    }
    let gone_ids: Vec<_> = gone.iter().filter_map(|&id| node_id(def, id)).collect();
    def.nodes.retain(|n| !gone.contains(&n.id));
    if let Some(meta) = def.preset_metadata.as_mut() {
        for binding in &mut meta.bindings {
            if let BindingTarget::Node { node_id, param } = &mut binding.target
                && let Some((_, object)) = moved.iter().find(|(cover, _)| cover == node_id)
            {
                *node_id = object.clone();
                *param = "foam_coverage".into();
            }
        }
        meta.bindings.retain(|binding| {
            !matches!(&binding.target, BindingTarget::Node { node_id, .. } if gone_ids.contains(node_id))
        });
    }
    !retired.is_empty()
}

/// Drop a saved surface's `node.smooth_lattice` passes, after flattening:
/// each `node.field_smooth_blend` smooths its own prior along x, y and z, so
/// the three axis passes before it and the wire into its old `smoothed` go
/// (their first pass read that same prior). Idempotent.
pub(crate) fn fold_smoothing_into_blend(def: &mut EffectGraphDef) -> bool {
    const RETIRED: &str = "node.smooth_lattice";
    let retired: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == RETIRED)
        .map(|n| n.id)
        .collect();
    def.wires
        .retain(|w| !retired.contains(&w.from_node) && !retired.contains(&w.to_node));
    def.nodes.retain(|n| n.type_id != RETIRED);
    !retired.is_empty()
}

/// The material a new Water used to get: blue-teal glass, tinted cyan,
/// absorbing over 10 cm and lit by teal scattering.
const STYLISED_WATER: [(&str, f32); 12] = [
    ("color_r", 0.3725),
    ("color_g", 0.7294),
    ("color_b", 1.0),
    ("roughness", 0.15),
    ("volume_attenuation_color_r", 0.35),
    ("volume_attenuation_color_g", 0.72),
    ("volume_attenuation_color_b", 0.8),
    ("volume_attenuation_distance", 0.1),
    ("volume_scattering_color_r", 0.15),
    ("volume_scattering_color_g", 0.5),
    ("volume_scattering_color_b", 0.6),
    ("volume_scattering_density", 0.6),
];

/// Give a saved material still on the old stylised water default the clear
/// water a new Water gets, after flattening. A material with any of those
/// values changed is the user's look and stays. Idempotent.
pub(crate) fn clear_stylised_water(def: &mut EffectGraphDef) -> bool {
    use crate::presets::gpu_flip::CLEAR_WATER;
    use manifold_core::effect_graph_def::SerializedParamValue;
    let mut changed = false;
    for node in def
        .nodes
        .iter_mut()
        .filter(|n| n.type_id == "node.pbr_material")
    {
        let stylised = STYLISED_WATER.iter().all(|&(name, old)| {
            matches!(node.params.get(name), Some(SerializedParamValue::Float { value }) if (value - old).abs() < 1e-3)
        });
        if !stylised {
            continue;
        }
        for (name, _) in STYLISED_WATER {
            node.params.remove(name);
        }
        for (name, value) in CLEAR_WATER {
            node.params
                .insert(name.to_owned(), SerializedParamValue::Float { value });
        }
        changed = true;
    }
    changed
}

/// Move a saved surface's wetness onto the wet map, after flattening: each
/// `node.surface_wetness` becomes a `node.wet_map` reading the particles its
/// display tiles' push-out draws, the solid on those tiles and the water's
/// cell size; its map feeds the water object's `wet_map` (a switch between
/// them becomes the u32 one). `node.surface_tiles` keeps no tiles, so the
/// wires into its `time` and `epoch` and its `keep_seconds` go. The wet map
/// keeps its marks as decals with no place on the ground, so a wire into its
/// old `extent` goes too. Idempotent.
pub(crate) fn wetness_onto_wet_map(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let tiles: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.surface_tiles")
        .map(|n| n.id)
        .collect();
    let maps: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.wet_map")
        .map(|n| n.id)
        .collect();
    let before = def.wires.len();
    def.wires.retain(|w| {
        !(tiles.contains(&w.to_node) && matches!(w.to_port.as_str(), "time" | "epoch")
            || maps.contains(&w.to_node) && w.to_port == "extent")
    });
    let mut changed = def.wires.len() != before;
    for node in def.nodes.iter_mut().filter(|n| tiles.contains(&n.id)) {
        changed |= node.params.remove("keep_seconds").is_some();
    }
    let wetness: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.surface_wetness")
        .map(|n| n.id)
        .collect();
    let source = |def: &EffectGraphDef, to: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == to && w.to_port == port)
            .map(|w| (w.from_node, w.from_port.clone()))
    };
    let type_of = |def: &EffectGraphDef, id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.clone())
    };
    for &id in &wetness {
        def.wires
            .retain(|w| !(w.to_node == id && w.to_port == "field"));
        let mut switches = Vec::new();
        for w in def.wires.iter_mut().filter(|w| w.from_node == id) {
            w.from_port = "map".into();
            if w.to_port == "field_wetness" {
                w.to_port = "wet_map".into();
            } else {
                switches.push(w.to_node);
            }
        }
        for &switch in &switches {
            if type_of(def, switch).as_deref() != Some("node.switch_array") {
                continue;
            }
            if let Some(node) = def.nodes.iter_mut().find(|n| n.id == switch) {
                node.type_id = "node.switch_indices".into();
            }
            for w in def
                .wires
                .iter_mut()
                .filter(|w| w.from_node == switch && w.to_port == "field_wetness")
            {
                w.to_port = "wet_map".into();
            }
        }
        // The display tiles this wetness was stored on, and the nodes they feed.
        let Some((table, _)) = source(def, id, "tiles") else {
            continue;
        };
        let fed = |def: &EffectGraphDef, type_id: &str| {
            def.wires
                .iter()
                .filter(|w| w.from_node == table && w.to_port == "tiles")
                .map(|w| w.to_node)
                .find(|&to| type_of(def, to).as_deref() == Some(type_id))
        };
        let mut new = Vec::new();
        if let Some(push_out) = fed(def, "node.push_out_of_solid") {
            new.push((push_out, "out".to_owned(), "particles"));
        }
        if let Some(solid) = fed(def, "node.liquid_solid_distance") {
            new.push((solid, "solid".to_owned(), "solid"));
        }
        if let Some((cell, port)) = source(def, table, "sim_cell_size") {
            new.push((cell, port, "sim_cell_size"));
        }
        for (from_node, from_port, to_port) in new {
            def.wires.push(EffectGraphWire {
                from_node,
                from_port,
                to_node: id,
                to_port: to_port.into(),
            });
        }
        if let Some(node) = def.nodes.iter_mut().find(|n| n.id == id) {
            node.type_id = "node.wet_map".into();
        }
        changed = true;
    }
    changed
}

/// Make a saved water's bubbles the silvery mirrors a new Water draws, after
/// flattening: a bubble material still at the old see-through look (0.35
/// transmission) takes the mirror and drops its screen-space density, which
/// nothing reads. Idempotent.
pub(crate) fn silver_bubbles(def: &mut EffectGraphDef) -> bool {
    use crate::presets::gpu_flip::SILVER_BUBBLE;
    use manifold_core::effect_graph_def::SerializedParamValue;
    let mut changed = false;
    for node in def
        .nodes
        .iter_mut()
        .filter(|n| n.type_id == "node.pbr_material" && n.node_id.as_str() == "bubble_material")
    {
        let see_through = matches!(
            node.params.get("transmission"),
            Some(SerializedParamValue::Float { value }) if (value - 0.35).abs() < 1e-3
        );
        if !see_through {
            continue;
        }
        node.params.remove("volume_particle_density");
        for (name, value) in SILVER_BUBBLE {
            node.params
                .insert(name.to_owned(), SerializedParamValue::Float { value });
        }
        changed = true;
    }
    changed
}

/// Unwire foam and spray from a saved water's display table: they draw as
/// particles and hold no display tiles. Idempotent.
pub(crate) fn whitewater_off_display_tiles(def: &mut EffectGraphDef) -> bool {
    let tiles: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.surface_tiles")
        .map(|n| n.id)
        .collect();
    let before = def.wires.len();
    def.wires.retain(|w| {
        !(tiles.contains(&w.to_node) && matches!(w.to_port.as_str(), "foam" | "spray"))
    });
    def.wires.len() != before
}

/// Wire the static colliders' trees beside every saved collider wire: each
/// wire carrying a GPU FLIP domain's `atlas` gains the domain's
/// `collider_nodes` and `collider_triangles` into the same node, so a Fixed
/// Collider's exact mesh reaches every reader. Matter's solver wires its own
/// trees (its grid passes read them through node.matter_static_solid). A
/// port already wired is kept. Idempotent.
pub(crate) fn wire_collider_trees(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let domains: Vec<u32> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == GPU_FLIP_DOMAIN_TYPE_ID)
        .map(|n| n.id)
        .collect();
    let mut added: Vec<EffectGraphWire> = Vec::new();
    for wire in def
        .wires
        .iter()
        .filter(|w| domains.contains(&w.from_node) && w.from_port == "atlas")
    {
        for port in ["collider_nodes", "collider_triangles"] {
            let wired = def
                .wires
                .iter()
                .chain(&added)
                .any(|w| w.to_node == wire.to_node && w.to_port == port);
            if !wired {
                added.push(EffectGraphWire {
                    from_node: wire.from_node,
                    from_port: port.into(),
                    to_node: wire.to_node,
                    to_port: port.into(),
                });
            }
        }
    }
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Wire the simulation cell into every display smoothing step and Close Gaps
/// saved without it, from the wire feeding the `sim_cell_size` of the
/// `node.particle_volume` on the same tiles, so both reach a simulation cell
/// at every Surface Detail. A port already wired is kept. Idempotent.
pub(crate) fn wire_display_stride(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let source = |node: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == port)
            .map(|w| (w.from_node, w.from_port.clone()))
    };
    let volumes: Vec<((u32, String), (u32, String))> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.particle_volume")
        .filter_map(|n| Some((source(n.id, "tiles")?, source(n.id, "sim_cell_size")?)))
        .collect();
    let added: Vec<EffectGraphWire> = def
        .nodes
        .iter()
        .filter(|n| {
            matches!(
                n.type_id.as_str(),
                "node.field_smooth_blend" | "node.field_close"
            )
        })
        .filter(|n| source(n.id, "sim_cell_size").is_none())
        .filter_map(|n| {
            let tiles = source(n.id, "tiles")?;
            let (_, (from_node, from_port)) = volumes.iter().find(|(t, _)| *t == tiles)?;
            Some(EffectGraphWire {
                from_node: *from_node,
                from_port: from_port.clone(),
                to_node: n.id,
                to_port: "sim_cell_size".into(),
            })
        })
        .collect();
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Shape Particle Blobs reads each particle's neighbours through the display
/// cells it was sorted into: a saved blob node without them takes the
/// cell ranges, tiles, pool, display cell and simulation cell of the Particle
/// Volume its blobs feed.
pub(crate) fn wire_blob_neighbours(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    const PORTS: [&str; 5] = [
        "cell_ranges",
        "tiles",
        "pool_tiles",
        "cell_size",
        "sim_cell_size",
    ];
    let source = |node: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == port)
            .map(|w| (w.from_node, w.from_port.clone()))
    };
    let added: Vec<EffectGraphWire> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.shape_particle_blobs")
        .filter(|n| source(n.id, "cell_ranges").is_none())
        .filter_map(|n| {
            let volume = def.wires.iter().find(|w| {
                w.from_node == n.id
                    && w.from_port == "blobs"
                    && w.to_port == "blobs"
                    && def
                        .nodes
                        .iter()
                        .any(|v| v.id == w.to_node && v.type_id == "node.particle_volume")
            })?;
            Some(PORTS.into_iter().filter_map(move |port| {
                let (from_node, from_port) = source(volume.to_node, port)?;
                (source(n.id, port).is_none()).then(|| EffectGraphWire {
                    from_node,
                    from_port,
                    to_node: n.id,
                    to_port: port.into(),
                })
            }))
        })
        .flatten()
        .collect();
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

/// Particle Volume gives the smoothing weights and the surface velocity from
/// its own blend, so a saved Surface Weights node folds into the Particle
/// Volume on the same tiles: the volume takes the blob node's counts and the
/// sorted particles the weights node read, every wire from the weights
/// node's outputs moves to the volume's, and the weights node goes.
pub(crate) fn fold_surface_weights(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let source = |def: &EffectGraphDef, node: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == port)
            .map(|w| (w.from_node, w.from_port.clone()))
    };
    let folds: Vec<(u32, u32, u32, (u32, String))> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.surface_weights")
        .filter_map(|weights| {
            let tiles = source(def, weights.id, "tiles")?;
            let particles = source(def, weights.id, "particles")?;
            let volume = def.nodes.iter().find(|v| {
                v.type_id == "node.particle_volume"
                    && source(def, v.id, "tiles").as_ref() == Some(&tiles)
            })?;
            let (blobs, _) = source(def, volume.id, "blobs")?;
            Some((weights.id, volume.id, blobs, particles))
        })
        .collect();
    let changed = !folds.is_empty();
    for (weights, volume, blobs, (from_node, from_port)) in folds {
        for wire in def.wires.iter_mut().filter(|w| w.from_node == weights) {
            wire.from_node = volume;
        }
        def.wires.retain(|w| w.to_node != weights);
        def.nodes.retain(|n| n.id != weights);
        for (from_node, from_port, to_port) in [
            (blobs, "counts".to_string(), "counts"),
            (from_node, from_port, "sorted"),
        ] {
            if source(def, volume, to_port).is_none() {
                def.wires.push(EffectGraphWire {
                    from_node,
                    from_port,
                    to_node: volume,
                    to_port: to_port.into(),
                });
            }
        }
    }
    changed
}

/// A saved liquid state takes the inflows' most births a tick from the GPU
/// FLIP domain that feeds its `epoch`, so its storage grows ahead of them.
/// Idempotent.
pub(crate) fn wire_state_emit_sites(def: &mut EffectGraphDef) -> bool {
    use manifold_core::effect_graph_def::EffectGraphWire;
    let kind = |id: u32| {
        def.nodes
            .iter()
            .find(|n| n.id == id)
            .map(|n| n.type_id.as_str())
    };
    let wired = |node: u32, port: &str| {
        def.wires
            .iter()
            .find(|w| w.to_node == node && w.to_port == port)
            .map(|w| w.from_node)
    };
    let added: Vec<EffectGraphWire> = def
        .nodes
        .iter()
        .filter(|n| n.type_id == "node.liquid_state" && wired(n.id, "emit_sites").is_none())
        .filter_map(|n| {
            let domain = wired(n.id, "epoch")?;
            (kind(domain) == Some(GPU_FLIP_DOMAIN_TYPE_ID)).then(|| EffectGraphWire {
                from_node: domain,
                from_port: "region_site_total".into(),
                to_node: n.id,
                to_port: "emit_sites".into(),
            })
        })
        .collect();
    let changed = !added.is_empty();
    def.wires.extend(added);
    changed
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_state_emit_sites",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 480,
        apply: wire_state_emit_sites,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "fold_surface_weights",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 470,
        apply: fold_surface_weights,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_blob_neighbours",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 460,
        apply: wire_blob_neighbours,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_display_stride",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 450,
        apply: wire_display_stride,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_collider_trees",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 440,
        apply: wire_collider_trees,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "whitewater_off_display_tiles",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 430,
        apply: whitewater_off_display_tiles,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_shaped_fill",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 420,
        apply: wire_shaped_fill,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "silver_bubbles",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 410,
        apply: silver_bubbles,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wetness_onto_wet_map",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 400,
        apply: wetness_onto_wet_map,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "clear_stylised_water",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 390,
        apply: clear_stylised_water,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "retire_foam_cover",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 395,
        apply: retire_foam_cover,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "fold_smoothing_into_blend",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 380,
        apply: fold_smoothing_into_blend,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "retire_redistance",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 370,
        apply: retire_redistance,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "wire_whitewater_removal",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 360,
        apply: wire_whitewater_removal,
    }
}

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "pool_whitewater_on_step_tiles",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 350,
        apply: pool_whitewater_on_step_tiles,
    }
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

inventory::submit! {
    manifold_node_engine::load::migration::GraphMigration {
        name: "retire_dense_liquid_faces",
        stage: manifold_node_engine::load::migration::MigrationStage::AfterFlatten,
        order: 340,
        apply: retire_dense_liquid_faces,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::EffectGraphNode;

    /// A saved whitewater step fed `bounds_lo_x` by a GPU FLIP domain gains
    /// the removal wires from that domain, once; a step with no domain feed,
    /// and a port already wired, are left alone.
    #[test]
    fn saved_whitewater_reads_the_domain_removal() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, port: &str, to| EffectGraphWire {
            from_node: from,
            from_port: port.into(),
            to_node: to,
            to_port: port.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID),
                bare_node(2, "node.whitewater_step"),
                bare_node(3, "node.whitewater_step"),
                bare_node(4, "node.value"),
            ],
            wires: vec![
                wire(1, "bounds_lo_x", 2),
                wire(4, "kill_height", 2),
                wire(4, "bounds_lo_x", 3),
            ],
        };
        assert!(wire_whitewater_removal(&mut def));
        let into = |to: u32, port: &str| {
            def.wires
                .iter()
                .filter(|w| w.to_node == to && w.to_port == port)
                .map(|w| w.from_node)
                .collect::<Vec<_>>()
        };
        for port in ["bounds_hi_x", "bounds_hi_y", "bounds_hi_z", "open_faces"] {
            assert_eq!(into(2, port), [1], "{port}");
            assert!(into(3, port).is_empty(), "{port}");
        }
        assert_eq!(into(2, "kill_height"), [4]);
        let migrated = def.wires.clone();
        assert!(!wire_whitewater_removal(&mut def));
        assert_eq!(def.wires, migrated);
    }

    /// A saved surface's re-distance goes, and the smoothing and weights it
    /// fed read the splat it read; a second run changes nothing.
    #[test]
    fn saved_surface_reads_the_splat_without_its_redistance() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.particle_volume"),
                bare_node(2, "node.redistance_lattice"),
                bare_node(3, "node.surface_weights"),
                bare_node(4, "node.field_smooth_blend"),
                bare_node(5, "node.surface_tiles"),
            ],
            wires: vec![
                wire(1, "levelset", 2, "levelset"),
                wire(5, "tiles", 2, "tiles"),
                wire(5, "tiles", 3, "tiles"),
                wire(2, "out", 3, "reference"),
                wire(2, "out", 4, "prior"),
                wire(2, "out", 4, "reference"),
            ],
        };
        assert!(retire_redistance(&mut def));
        assert!(def.nodes.iter().all(|n| n.id != 2));
        assert_eq!(
            def.wires,
            [
                wire(5, "tiles", 3, "tiles"),
                wire(1, "levelset", 3, "reference"),
                wire(1, "levelset", 4, "prior"),
                wire(1, "levelset", 4, "reference"),
            ]
        );
        let migrated = def.clone();
        assert!(!retire_redistance(&mut def));
        assert_eq!(def, migrated);
    }

    /// A saved water's foam cover and the switch it fed go, and the water
    /// object takes the foam particles; a second run changes nothing.
    #[test]
    fn saved_foam_switch_reads_the_particles_without_the_cover() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.interpolate_particle_frames"),
                bare_node(2, "node.foam_cover"),
                bare_node(3, "node.switch_array"),
                bare_node(4, "node.surface_tiles"),
                bare_node(5, "node.scene_object"),
            ],
            wires: vec![
                wire(1, "out", 4, "foam"),
                wire(1, "out", 2, "particles"),
                wire(4, "tiles", 2, "tiles"),
                wire(2, "cover", 3, "in_0"),
                wire(3, "out", 5, "field_foam"),
            ],
        };
        assert!(retire_foam_cover(&mut def));
        assert!(def.nodes.iter().all(|n| n.id != 2 && n.id != 3));
        assert_eq!(
            def.wires,
            [wire(1, "out", 4, "foam"), wire(1, "out", 5, "field_foam")]
        );
        let migrated = def.clone();
        assert!(!retire_foam_cover(&mut def));
        assert_eq!(def, migrated);
    }

    /// A saved smoothing step's three axis passes go with every wire they
    /// carried; the blend keeps its prior, weights, reference and tiles. A
    /// second run changes nothing.
    #[test]
    fn saved_smoothing_folds_into_the_blend() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.particle_volume"),
                bare_node(2, "node.smooth_lattice"),
                bare_node(3, "node.smooth_lattice"),
                bare_node(4, "node.smooth_lattice"),
                bare_node(5, "node.field_smooth_blend"),
                bare_node(6, "node.surface_weights"),
                bare_node(7, "node.surface_tiles"),
            ],
            wires: vec![
                wire(1, "levelset", 2, "levelset"),
                wire(2, "smoothed", 3, "levelset"),
                wire(3, "smoothed", 4, "levelset"),
                wire(4, "smoothed", 5, "smoothed"),
                wire(1, "levelset", 5, "prior"),
                wire(6, "weights", 5, "weights"),
                wire(1, "levelset", 5, "reference"),
                wire(7, "tiles", 2, "tiles"),
                wire(7, "tiles", 5, "tiles"),
            ],
        };
        assert!(fold_smoothing_into_blend(&mut def));
        let ids: Vec<u32> = def.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [1, 5, 6, 7]);
        assert_eq!(
            def.wires,
            [
                wire(1, "levelset", 5, "prior"),
                wire(6, "weights", 5, "weights"),
                wire(1, "levelset", 5, "reference"),
                wire(7, "tiles", 5, "tiles"),
            ]
        );
        let migrated = def.clone();
        assert!(!fold_smoothing_into_blend(&mut def));
        assert_eq!(def, migrated);
    }

    /// A saved material on the old stylised default, its colour stored at f32
    /// precision as a colour picker leaves it, turns into the clear water a
    /// new Water gets. One with a single value changed keeps the user's look,
    /// and a second run changes nothing.
    #[test]
    fn saved_stylised_water_turns_clear() {
        use crate::presets::gpu_flip::CLEAR_WATER;
        use manifold_core::effect_graph_def::SerializedParamValue;
        let material = |id, roughness: f32| {
            let mut node = bare_node(id, "node.pbr_material");
            for (name, value) in STYLISED_WATER {
                node.params
                    .insert(name.to_owned(), SerializedParamValue::Float { value });
            }
            for (name, value) in [
                ("color_r", 0.372_549_03),
                ("color_g", 0.729_411_8),
                ("roughness", roughness),
            ] {
                node.params
                    .insert(name.to_owned(), SerializedParamValue::Float { value });
            }
            node
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![material(1, 0.15), material(2, 0.4)],
            wires: Vec::new(),
        };
        let users = def.nodes[1].clone();
        assert!(clear_stylised_water(&mut def));
        let clear: std::collections::BTreeMap<String, SerializedParamValue> = CLEAR_WATER
            .iter()
            .map(|&(name, value)| (name.to_owned(), SerializedParamValue::Float { value }))
            .collect();
        assert_eq!(def.nodes[0].params, clear);
        assert_eq!(def.nodes[1], users);
        let migrated = def.clone();
        assert!(!clear_stylised_water(&mut def));
        assert_eq!(def, migrated);
    }

    /// A saved surface's wetness moves onto the wet map: retyped and rewired
    /// from the tiles' push-out, solid and cell size, its map through a u32
    /// switch into the water's `wet_map`; the tiles lose their clock wires and
    /// a wet map its extent. A second run changes nothing.
    #[test]
    fn saved_wetness_moves_onto_the_wet_map() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.gpu_flip_domain"),
                bare_node(2, "node.surface_tiles"),
                bare_node(3, "node.liquid_solid_distance"),
                bare_node(4, "node.push_out_of_solid"),
                bare_node(5, "node.surface_wetness"),
                bare_node(6, "node.switch_array"),
                bare_node(7, "node.scene_object"),
                bare_node(8, "node.field_smooth_blend"),
                bare_node(9, "node.wet_map"),
            ],
            wires: vec![
                wire(1, "cell_size", 2, "sim_cell_size"),
                wire(1, "display_time", 2, "time"),
                wire(1, "epoch", 2, "epoch"),
                wire(2, "tiles", 3, "tiles"),
                wire(2, "tiles", 4, "tiles"),
                wire(2, "tiles", 5, "tiles"),
                wire(8, "out", 5, "field"),
                wire(1, "display_time", 5, "time"),
                wire(5, "wetness", 6, "in_0"),
                wire(6, "out", 7, "field_wetness"),
                wire(2, "extent", 9, "extent"),
            ],
        };
        assert!(wetness_onto_wet_map(&mut def));
        let type_of = |id: u32| {
            def.nodes
                .iter()
                .find(|n| n.id == id)
                .unwrap()
                .type_id
                .clone()
        };
        assert_eq!(type_of(5), "node.wet_map");
        assert_eq!(type_of(6), "node.switch_indices");
        let mut wires: Vec<_> = def
            .wires
            .iter()
            .map(|w| {
                (
                    w.from_node,
                    w.from_port.as_str(),
                    w.to_node,
                    w.to_port.as_str(),
                )
            })
            .collect();
        wires.sort();
        assert_eq!(
            wires,
            vec![
                (1, "cell_size", 2, "sim_cell_size"),
                (1, "cell_size", 5, "sim_cell_size"),
                (1, "display_time", 5, "time"),
                (2, "tiles", 3, "tiles"),
                (2, "tiles", 4, "tiles"),
                (2, "tiles", 5, "tiles"),
                (3, "solid", 5, "solid"),
                (4, "out", 5, "particles"),
                (5, "map", 6, "in_0"),
                (6, "out", 7, "wet_map"),
            ]
        );
        assert!(!wetness_onto_wet_map(&mut def));
    }

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
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.liquid_state"),
                bare_node(2, "node.liquid_frame"),
                bare_node(3, "node.interpolate_particle_frames"),
                bare_node(4, "node.interpolate_particle_frames"),
            ],
            wires: vec![
                wire(1, "out", 2, "particles"),
                wire(1, "foam_particles", 3, "particles_b"),
                wire(2, "blend", 3, "blend"),
                wire(2, "span", 3, "span"),
                wire(1, "spray_particles", 4, "particles_b"),
                wire(2, "blend", 4, "blend"),
                wire(2, "span", 4, "span"),
                wire(2, "count_b", 4, "count_b"),
            ],
        }
    }

    #[test]
    fn retained_whitewater_migration_is_exact_idempotent_and_survives_reload() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        // Shared frame: both classes move onto it, the authored count stays.
        let mut def = saved_whitewater();
        assert!(wire_retained_whitewater(&mut def));
        for (class, node) in [("foam", 3), ("spray", 4)] {
            assert!(def.wires.contains(&wire(
                1,
                &format!("{class}_particles"),
                2,
                &format!("{class}_in")
            )));
            assert!(
                def.wires
                    .contains(&wire(2, &format!("{class}_b"), node, "particles_b"))
            );
            assert_eq!(
                def.wires
                    .iter()
                    .filter(|w| w.to_node == node && w.to_port == "particles_b")
                    .count(),
                1
            );
        }
        assert!(def.wires.contains(&wire(2, "count_b", 4, "count_b")));
        // Idempotent, and a save/reload stays migrated.
        let once = def.clone();
        assert!(!wire_retained_whitewater(&mut def));
        assert_eq!(def, once);
        let mut reloaded: EffectGraphDef =
            serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
        assert!(!wire_retained_whitewater(&mut reloaded));
        assert_eq!(reloaded, once);

        // A conflicting explicit class input on the frame is preserved and
        // that interpolator left alone.
        let mut conflict = saved_whitewater();
        conflict.nodes.push(bare_node(5, "node.liquid_state"));
        conflict.wires.push(wire(5, "foam_particles", 2, "foam_in"));
        assert!(wire_retained_whitewater(&mut conflict));
        assert!(
            conflict
                .wires
                .contains(&wire(1, "foam_particles", 3, "particles_b"))
        );
        assert!(
            conflict
                .wires
                .contains(&wire(5, "foam_particles", 2, "foam_in"))
        );
        assert!(
            conflict
                .wires
                .contains(&wire(2, "spray_b", 4, "particles_b"))
        );

        // Blend and span from different frames, or a frame reading another
        // state: no match.
        for custom in [
            {
                let mut d = saved_whitewater();
                d.nodes.push(bare_node(6, "node.liquid_frame"));
                d.wires.retain(|w| !(w.to_node == 3 && w.to_port == "span"));
                d.wires.push(wire(6, "span", 3, "span"));
                d.wires.retain(|w| w.to_node != 4);
                d
            },
            {
                let mut d = saved_whitewater();
                d.nodes.push(bare_node(7, "node.liquid_state"));
                d.wires
                    .retain(|w| !(w.to_node == 2 && w.to_port == "particles"));
                d.wires.push(wire(7, "out", 2, "particles"));
                d
            },
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
        }))
        .unwrap();
        let mut flat = manifold_core::flatten::flatten_groups(&nested).unwrap();
        assert!(wire_retained_whitewater(&mut flat));
        assert_eq!(
            flat.wires
                .iter()
                .filter(|w| w.to_port == "particles_b"
                    && (w.from_port == "foam_b" || w.from_port == "spray_b"))
                .count(),
            2
        );
        assert!(!wire_retained_whitewater(&mut flat));
    }

    #[test]
    fn liquid_frame_cursor_migration_binds_the_frame_clock() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        // 1 GPU FLIP domain, 2 Matter domain, 3 frame. The Matter domain feeds
        // another input; the clock is the epoch's source.
        let base = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID),
                bare_node(2, MATTER_DOMAIN_TYPE_ID),
                bare_node(3, "node.liquid_frame"),
            ],
            wires: vec![
                wire(2, "closed_faces", 3, "closed_faces"),
                wire(1, "display_time", 3, "display_time"),
                wire(1, "epoch", 3, "epoch"),
            ],
        };
        let expected = [
            wire(1, "display_cursor", 3, "display_cursor"),
            wire(1, "dropped_seconds", 3, "dropped_seconds"),
        ];
        // Every order of the authored wires gives the same result.
        let orders = [
            [0, 1, 2],
            [0, 2, 1],
            [1, 0, 2],
            [1, 2, 0],
            [2, 0, 1],
            [2, 1, 0],
        ];
        for order in orders {
            let mut def = base.clone();
            def.wires = order.iter().map(|&i| base.wires[i].clone()).collect();
            assert!(wire_liquid_frame_cursor(&mut def));
            for w in &expected {
                assert_eq!(def.wires.iter().filter(|x| *x == w).count(), 1, "{order:?}");
            }
            assert!(
                !def.wires
                    .iter()
                    .any(|w| w.from_node == 2 && w.to_port == "display_cursor")
            );
            // Idempotent, and a save/reload stays migrated.
            let once = def.clone();
            assert!(!wire_liquid_frame_cursor(&mut def));
            let mut reloaded: EffectGraphDef =
                serde_json::from_str(&serde_json::to_string(&def).unwrap()).unwrap();
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
        assert_eq!(
            authored
                .wires
                .iter()
                .filter(|w| w.to_port == "display_cursor")
                .count(),
            1
        );
        assert!(
            authored
                .wires
                .contains(&wire(4, "value", 3, "display_cursor"))
        );
        // Nested groups: the installation seam flattens first.
        let nested: EffectGraphDef = serde_json::from_value(serde_json::json!({
            "version": 1,
            "nodes": [{ "id": 1, "typeId": "group", "handle": "outer", "group": {
                "interface": { "inputs": [], "outputs": [] },
                "nodes": base.nodes, "wires": base.wires
            }}], "wires": []
        }))
        .unwrap();
        let mut flat = manifold_core::flatten::flatten_groups(&nested).unwrap();
        assert!(wire_liquid_frame_cursor(&mut flat));
        assert_eq!(
            flat.wires
                .iter()
                .filter(|w| w.to_port == "display_cursor")
                .count(),
            1
        );
    }

    #[test]
    fn live_clock_graph_wires_are_explicit_idempotent_and_preserve_authored_duration() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID),
                bare_node(2, "node.liquid_state"),
                bare_node(3, "node.gpu_flip_step"),
                bare_node(4, "node.whitewater_step"),
                bare_node(5, "node.scalar"),
            ],
            wires: vec![
                wire(1, "ticks", 2, "ticks"),
                wire(2, "out", 3, "particles"),
                wire(3, "out", 2, "in"),
                wire(3, "faces", 4, "faces"),
                wire(5, "out", 4, "dt"),
            ],
        };
        assert!(wire_liquid_intervals(&mut def));
        assert!(
            def.wires
                .contains(&wire(1, "interval_duration", 3, "interval_duration"))
        );
        assert!(def.wires.contains(&wire(1, "live_hits", 3, "live_hits")));
        assert!(
            def.wires
                .contains(&wire(3, "clock_status", 2, "clock_status_in"))
        );
        assert!(def.wires.contains(&wire(2, "identity", 3, "identity")));
        assert!(
            def.wires
                .contains(&wire(3, "identity_out", 2, "identity_in"))
        );
        assert_eq!(
            def.wires
                .iter()
                .filter(|w| w.to_node == 4 && w.to_port == "dt")
                .count(),
            1
        );
        assert!(def.wires.contains(&wire(5, "out", 4, "dt")));
        assert!(!wire_liquid_intervals(&mut def));
    }

    #[test]
    fn dropped_time_wires_are_added_once_and_preserve_authored_inputs() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        for (domain, state) in [
            (GPU_FLIP_DOMAIN_TYPE_ID, "node.liquid_state"),
            (MATTER_DOMAIN_TYPE_ID, "node.matter_state"),
        ] {
            let authored = wire(4, "out", 3, "dropped_seconds");
            let mut def = EffectGraphDef {
                version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
                name: None,
                description: None,
                preset_metadata: None,
                scene_modifiers: Vec::new(),
                nodes: vec![
                    bare_node(1, domain),
                    bare_node(2, state),
                    bare_node(3, state),
                    bare_node(4, "node.scalar"),
                    bare_node(5, state),
                ],
                wires: vec![
                    wire(1, "ticks", 2, "ticks"),
                    wire(1, "ticks", 3, "ticks"),
                    authored.clone(),
                ],
            };
            assert!(wire_liquid_intervals(&mut def));
            assert!(
                def.wires
                    .contains(&wire(1, "dropped_seconds", 2, "dropped_seconds"))
            );
            assert!(def.wires.contains(&authored));
            for state in [2, 3] {
                assert_eq!(
                    def.wires
                        .iter()
                        .filter(|w| w.to_node == state && w.to_port == "dropped_seconds")
                        .count(),
                    1
                );
            }
            assert!(
                !def.wires.iter().any(|w| w.to_node == 5),
                "an unowned state has no inferred clock"
            );
            let migrated = def.wires.clone();
            assert!(!wire_liquid_intervals(&mut def));
            assert_eq!(def.wires, migrated);
        }
    }

    #[test]
    fn identity_wires_preserve_authored_inputs_and_follow_each_steps_state() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        let authored = wire(5, "out", 4, "identity");
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.liquid_state"),
                bare_node(2, "node.gpu_flip_step"),
                bare_node(3, "node.liquid_state"),
                bare_node(4, "node.gpu_flip_step"),
                bare_node(5, "node.scalar"),
                bare_node(6, "node.gpu_flip_step"),
                bare_node(7, "node.liquid_stats"),
                bare_node(8, "node.liquid_stats"),
                bare_node(9, "node.liquid_stats"),
            ],
            wires: vec![
                wire(1, "out", 2, "particles"),
                wire(3, "out", 4, "particles"),
                authored.clone(),
                wire(2, "out", 7, "particles"),
                wire(4, "out", 8, "particles"),
                wire(5, "out", 9, "particles"),
                wire(6, "identity_out", 8, "identity"),
            ],
        };
        assert!(wire_liquid_intervals(&mut def));
        assert!(def.wires.contains(&wire(1, "identity", 2, "identity")));
        assert!(
            def.wires
                .contains(&wire(2, "identity_out", 1, "identity_in"))
        );
        assert!(def.wires.contains(&authored));
        for step in [2, 4] {
            assert_eq!(
                def.wires
                    .iter()
                    .filter(|w| w.to_node == step && w.to_port == "identity")
                    .count(),
                1
            );
        }
        assert!(
            !def.wires.iter().any(|w| w.to_node == 6),
            "no state is inferred for an unconnected step"
        );
        // Stats take the identity of the step they measure, and keep one they
        // were given.
        assert!(def.wires.contains(&wire(2, "identity_out", 7, "identity")));
        assert!(def.wires.contains(&wire(6, "identity_out", 8, "identity")));
        assert!(
            !def.wires
                .iter()
                .any(|w| w.to_node == 9 && w.to_port == "identity"),
            "stats that measure no step get no identity"
        );
        let migrated = def.wires.clone();
        assert!(!wire_liquid_intervals(&mut def));
        assert_eq!(def.wires, migrated);
    }

    #[test]
    fn whitewater_obstacle_source_uses_accepted_duration_without_overwriting_wires() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        for domain in [GPU_FLIP_DOMAIN_TYPE_ID, MATTER_DOMAIN_TYPE_ID] {
            let mut def = EffectGraphDef {
                version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
                name: None,
                description: None,
                preset_metadata: None,
                scene_modifiers: Vec::new(),
                nodes: vec![
                    bare_node(1, domain),
                    bare_node(2, "node.whitewater_obstacle_source"),
                    bare_node(3, "node.whitewater_obstacle_source"),
                    bare_node(4, "node.scalar"),
                ],
                wires: vec![
                    wire(1, "bodies", 2, "bodies"),
                    wire(1, "bodies", 3, "bodies"),
                    wire(4, "out", 3, "tick_seconds"),
                ],
            };
            assert!(wire_liquid_intervals(&mut def));
            assert!(
                def.wires
                    .contains(&wire(1, "interval_duration", 2, "tick_seconds"))
            );
            assert!(def.wires.contains(&wire(4, "out", 3, "tick_seconds")));
            assert_eq!(
                def.wires
                    .iter()
                    .filter(|w| w.to_node == 3 && w.to_port == "tick_seconds")
                    .count(),
                1
            );
            assert!(!wire_liquid_intervals(&mut def));
        }
    }

    #[test]
    fn dense_liquid_faces_retire_and_pooled_readers_gain_the_slot_table() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        // State 1, step 2, frame 3, state-fed components 4 and 5, whitewater 6,
        // an axis component 7 fed by the step, the domain 8.
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, "node.liquid_state"),
                bare_node(2, "node.gpu_flip_step"),
                bare_node(3, "node.liquid_frame"),
                bare_node(4, "node.face_sample_component"),
                bare_node(5, "node.face_sample_component"),
                bare_node(6, "node.whitewater_step"),
                bare_node(7, "node.face_sample_component"),
                bare_node(8, GPU_FLIP_DOMAIN_TYPE_ID),
            ],
            wires: vec![
                wire(2, "faces", 1, "faces_in"),
                wire(1, "faces", 4, "faces"),
                wire(1, "faces", 5, "faces"),
                wire(8, "nodes_x", 4, "nodes_x"),
                wire(4, "out", 3, "face_u_in"),
                wire(5, "out", 3, "face_v_in"),
                wire(3, "face_cells_x", 6, "face_cells_x"),
                wire(3, "particles_b", 6, "particles"),
                wire(2, "face_cells_x", 6, "face_cells_x"),
                wire(2, "distance", 6, "distance"),
                wire(2, "faces", 6, "faces"),
                wire(2, "faces", 7, "faces"),
                wire(7, "out", 6, "face_u"),
                wire(1, "out", 3, "particles"),
            ],
        };
        assert!(retire_dense_liquid_faces(&mut def));
        let ids: Vec<u32> = def.nodes.iter().map(|n| n.id).collect();
        assert_eq!(ids, [1, 2, 3, 6, 7, 8], "only the state-fed components go");
        for gone in [
            wire(2, "faces", 1, "faces_in"),
            wire(4, "out", 3, "face_u_in"),
            wire(3, "face_cells_x", 6, "face_cells_x"),
        ] {
            assert!(!def.wires.contains(&gone), "{gone:?}");
        }
        for kept in [
            wire(2, "face_cells_x", 6, "face_cells_x"),
            wire(3, "particles_b", 6, "particles"),
            wire(2, "faces", 6, "faces"),
            wire(7, "out", 6, "face_u"),
            wire(1, "out", 3, "particles"),
        ] {
            assert!(def.wires.contains(&kept), "{kept:?}");
        }
        for reader in [6, 7] {
            for port in ["tiles", "pool_tiles"] {
                assert_eq!(
                    def.wires
                        .iter()
                        .filter(|w| w.from_node == 2
                            && w.from_port == port
                            && w.to_node == reader
                            && w.to_port == port)
                        .count(),
                    1,
                    "{reader}.{port}"
                );
            }
        }
        let migrated = def.clone();
        assert!(!retire_dense_liquid_faces(&mut def));
        assert_eq!(def, migrated);
    }

    #[test]
    fn whitewater_moves_onto_the_step_tiles() {
        use manifold_core::effect_graph_def::EffectGraphWire;
        let wire = |from, output: &str, to, input: &str| EffectGraphWire {
            from_node: from,
            from_port: output.into(),
            to_node: to,
            to_port: input.into(),
        };
        // Domain 1, step 2, obstacle source 3, whitewater 4, the native solid
        // lattice 5.
        let mut def = EffectGraphDef {
            version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
            name: None,
            description: None,
            preset_metadata: None,
            scene_modifiers: Vec::new(),
            nodes: vec![
                bare_node(1, GPU_FLIP_DOMAIN_TYPE_ID),
                bare_node(2, "node.gpu_flip_step"),
                bare_node(3, "node.whitewater_obstacle_source"),
                bare_node(4, "node.whitewater_step"),
                bare_node(5, "node.liquid_solid_distance"),
            ],
            wires: vec![
                wire(1, "cell_size", 2, "cell_size"),
                wire(2, "distance", 4, "distance"),
                wire(2, "faces", 4, "faces"),
                wire(2, "substep_schedule", 4, "substep_schedule"),
                wire(2, "substep_u", 4, "substep_u"),
                wire(2, "grid_bounds", 4, "grid_bounds"),
                wire(2, "face_valid_layers", 4, "face_valid_layers"),
                wire(5, "solid", 4, "solid"),
                wire(3, "solid", 4, "obstacle_source"),
                wire(1, "mesh_min_x", 3, "lattice_min_x"),
                wire(1, "mesh_nodes_x", 3, "nodes_x"),
                wire(1, "bodies", 3, "bodies"),
            ],
        };
        assert!(pool_whitewater_on_step_tiles(&mut def));
        for gone in [
            wire(2, "substep_u", 4, "substep_u"),
            wire(2, "grid_bounds", 4, "grid_bounds"),
            wire(2, "face_valid_layers", 4, "face_valid_layers"),
            wire(5, "solid", 4, "solid"),
            wire(3, "solid", 4, "obstacle_source"),
            wire(1, "mesh_min_x", 3, "lattice_min_x"),
            wire(1, "mesh_nodes_x", 3, "nodes_x"),
        ] {
            assert!(!def.wires.contains(&gone), "{gone:?}");
        }
        for kept in [
            wire(2, "distance", 4, "distance"),
            wire(2, "substep_schedule", 4, "substep_schedule"),
            wire(1, "bodies", 3, "bodies"),
            wire(2, "substep_faces", 4, "substep_faces"),
            wire(3, "distance", 4, "solid"),
            wire(3, "source", 4, "obstacle_source"),
            wire(2, "tiles", 3, "tiles"),
            wire(2, "pool_tiles", 3, "pool_tiles"),
            wire(1, "cell_size", 4, "cell_size"),
            wire(1, "bounds_lo_x", 4, "bounds_lo_x"),
            wire(1, "bounds_lo_z", 4, "bounds_lo_z"),
        ] {
            assert!(def.wires.contains(&kept), "{kept:?}");
        }
        assert_eq!(def.wires.len(), 14);
        let migrated = def.clone();
        assert!(!pool_whitewater_on_step_tiles(&mut def));
        assert_eq!(def, migrated);
    }
}
