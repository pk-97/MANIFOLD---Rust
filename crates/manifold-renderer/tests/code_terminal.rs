//! Bounded acceptance coverage for the Code Terminal effect.
//!
//! The CPU gate checks the on-disk contract and exercises the shared binding
//! path.  The GPU gate renders one 960×540 fixture through the production
//! graph executor.  It deliberately samples a small set of control values and
//! two beat positions instead of becoming a thumbnail/render sweep.

use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::params::{Param, ParamManifest};
use manifold_renderer::node_graph::loaded_preset_view_by_id;
use manifold_renderer::node_graph::mesh_change::PreparedMeshRules;
use manifold_renderer::node_graph::{
    BoundGraph, EffectGraphDefExt, PrimitiveRegistry, ResolvedBinding, compile,
};

const PRESET_JSON: &str = include_str!("../assets/effect-presets/CodeTerminal.json");

fn preset() -> EffectGraphDef {
    serde_json::from_str(PRESET_JSON).expect("CodeTerminal JSON parses")
}

fn manifest(def: &EffectGraphDef, values: &[(&str, f32)]) -> ParamManifest {
    let specs = def
        .preset_metadata
        .as_ref()
        .expect("CodeTerminal metadata")
        .params
        .iter()
        .map(|spec| {
            let value = values
                .iter()
                .find(|(id, _)| *id == spec.id)
                .map(|(_, value)| *value)
                .unwrap_or(spec.default_value);
            let mut param = Param::bundled(spec.clone());
            param.value = value;
            param.base = value;
            param
        })
        .collect();
    ParamManifest::from_params(specs)
}

fn binding_graph(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
) -> (manifold_renderer::node_graph::Graph, BoundGraph) {
    let mut graph = def
        .clone()
        .into_graph(registry, &PreparedMeshRules::default())
        .expect("CodeTerminal graph instantiates");
    let view = loaded_preset_view_by_id(&manifold_core::PresetTypeId::new("CodeTerminal"))
        .expect("CodeTerminal loaded view");
    let node_map = graph
        .nodes()
        .map(|node| (node.node_id.clone(), node.id))
        .collect::<Vec<_>>();
    let bindings = view
        .bindings
        .iter()
        .map(|binding| {
            ResolvedBinding::from_static(binding, &node_map)
                .unwrap_or_else(|| panic!("binding {} resolves", binding.id))
        })
        .collect();
    let bound = BoundGraph::new(bindings, &mut graph, Some(def));
    (graph, bound)
}

#[test]
fn code_terminal_roundtrip_compiles_and_resolves_all_controls() {
    let def = preset();
    let encoded = serde_json::to_string(&def).expect("CodeTerminal serializes");
    let roundtrip: EffectGraphDef =
        serde_json::from_str(&encoded).expect("CodeTerminal roundtrip parses");
    assert_eq!(roundtrip, def, "preset JSON is serde-stable");

    let registry = PrimitiveRegistry::with_builtin();
    let (mut graph, mut bound) = binding_graph(&roundtrip, &registry);
    compile(&graph).expect("CodeTerminal graph compiles");
    assert!(
        roundtrip
            .nodes
            .iter()
            .any(|node| node.type_id == "node.terminal_stream")
    );
    assert!(
        roundtrip
            .nodes
            .iter()
            .any(|node| node.type_id == "node.glyph_atlas")
    );
    assert!(
        roundtrip
            .nodes
            .iter()
            .any(|node| node.type_id == "node.render_glyph_grid")
    );

    let values = manifest(
        &roundtrip,
        &[
            ("erosion", 0.47),
            ("text_size", 31.0),
            ("activity", 2.0),
            ("tonal_bias", -0.35),
            ("colour", 2.0),
        ],
    );
    bound.apply(&mut graph, &values);
    assert_eq!(bound.bindings.len(), 5, "every card control has a route");
    for (node_id, param, expected) in [
        ("erosion_low", "a", 0.47),
        ("terminal", "text_size", 31.0),
        ("terminal", "activity", 2.0),
        ("bias", "a", -0.35),
        ("palette", "selector", 2.0),
    ] {
        let instance = graph
            .instance_by_node_id(&manifold_core::NodeId::new(node_id))
            .expect("bound target node exists");
        let node = graph.get_node(instance).expect("bound target node live");
        let actual = match node.params.get(param).expect("bound target param") {
            manifold_renderer::node_graph::ParamValue::Float(value) => *value,
            value => panic!("{node_id}.{param} has unexpected value {value:?}"),
        };
        assert!(
            (actual - expected).abs() < 1e-5,
            "{node_id}.{param} binding"
        );
    }
}

#[cfg(feature = "gpu-proofs")]
mod gpu {
    use super::*;
    use manifold_renderer::node_graph::{
        Backend, Executor, FrameTime, MetalBackend, StateStore, pre_allocate_resources,
    };
    const W: u32 = 960;
    const H: u32 = 540;
    const FMT: manifold_gpu::GpuTextureFormat = manifold_gpu::GpuTextureFormat::Rgba16Float;
    use half::f16;
    use manifold_core::{Beats, Seconds};
    use manifold_gpu::{
        GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureUsage,
    };
    use manifold_renderer::gpu_encoder::GpuEncoder;
    use manifold_renderer::headless_readback::{
        encode_rgba8_png, readback_raw_halves, readback_srgb_rgba8,
    };
    use manifold_renderer::node_graph::freeze::install::fused_view_for;
    use manifold_renderer::node_graph::loaded_preset_view_by_id;
    use manifold_renderer::render_target::RenderTarget;

    #[derive(Clone, Copy)]
    struct Controls {
        erosion: f32,
        text_size: f32,
        activity: f32,
        tonal_bias: f32,
        colour: f32,
    }

    impl Controls {
        fn defaults() -> Self {
            Self {
                erosion: 1.0,
                text_size: 24.0,
                activity: 1.0,
                tonal_bias: 0.0,
                colour: 1.0,
            }
        }

        fn pairs(self) -> [(&'static str, f32); 5] {
            [
                ("erosion", self.erosion),
                ("text_size", self.text_size),
                ("activity", self.activity),
                ("tonal_bias", self.tonal_bias),
                ("colour", self.colour),
            ]
        }
    }

    const RECT_W: u32 = W / 5;
    const RECT_H: u32 = H / 3;

    fn source_frame(x: u32, y: u32) -> Vec<u8> {
        let mut halves = Vec::with_capacity((W * H * 4) as usize);
        for row in 0..H {
            for column in 0..W {
                let bright = (x..x + RECT_W).contains(&column) && (y..y + RECT_H).contains(&row);
                let value = if bright { 1.0 } else { 0.02 };
                halves.extend([value, value, value, 1.0].map(f16::from_f32));
            }
        }
        unsafe {
            std::slice::from_raw_parts(
                halves.as_ptr().cast::<u8>(),
                std::mem::size_of_val(halves.as_slice()),
            )
        }
        .to_vec()
    }

    fn fixture(device: &GpuDevice) -> (GpuTexture, Vec<u8>) {
        let mut halves = Vec::with_capacity((W * H * 4) as usize);
        for y in 0..H {
            for x in 0..W {
                let alpha = if (W / 3..W / 2).contains(&x) && y > H / 4 {
                    0.35
                } else {
                    1.0
                };
                let source = [
                    (0.08 + 0.72 * x as f32 / W as f32) * alpha,
                    (0.05 + 0.75 * y as f32 / H as f32) * alpha,
                    (0.12 + 0.55 * ((x + y) % 97) as f32 / 97.0) * alpha,
                    alpha,
                ];
                halves.extend(source.map(f16::from_f32));
            }
        }
        let bytes = unsafe {
            std::slice::from_raw_parts(
                halves.as_ptr().cast::<u8>(),
                std::mem::size_of_val(halves.as_slice()),
            )
        }
        .to_vec();
        let texture = device.create_texture(&GpuTextureDesc {
            width: W,
            height: H,
            depth: 1,
            format: FMT,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::CPU_UPLOAD
                | GpuTextureUsage::SHADER_READ
                | GpuTextureUsage::COPY_SRC,
            label: "code-terminal-acceptance-input",
            mip_levels: 1,
        });
        device.upload_texture(&texture, &bytes);
        (texture, bytes)
    }

    fn source_affected_rows(y: u32, rows: usize) -> std::ops::Range<usize> {
        let start = (y as usize * rows / H as usize).min(rows.saturating_sub(1));
        let end = ((y + RECT_H) as usize * rows / H as usize).clamp(start + 1, rows);
        start..end
    }

    struct Harness {
        device: std::sync::Arc<GpuDevice>,
        input: GpuTexture,
        graph: manifold_renderer::node_graph::Graph,
        plan: manifold_renderer::node_graph::ExecutionPlan,
        bound: BoundGraph,
        executor: Executor,
        state: StateStore,
        output_slot: manifold_renderer::node_graph::Slot,
        cells_resource: manifold_renderer::node_graph::ResourceId,
        columns_resource: manifold_renderer::node_graph::ResourceId,
        rows_resource: manifold_renderer::node_graph::ResourceId,
    }

    impl Harness {
        fn new(
            device: std::sync::Arc<GpuDevice>,
            def: &EffectGraphDef,
            input: &GpuTexture,
            fused: bool,
        ) -> Self {
            let registry = PrimitiveRegistry::with_builtin();
            let base = loaded_preset_view_by_id(&manifold_core::PresetTypeId::new("CodeTerminal"))
                .expect("CodeTerminal loaded view");
            let (render_def, bindings, mesh_rules) = if fused {
                let view = fused_view_for(def, base).expect("CodeTerminal has a fused region");
                (
                    view.canonical_def.clone(),
                    view.bindings.clone(),
                    view.mesh_rules.clone(),
                )
            } else {
                (
                    base.canonical_def.clone(),
                    base.bindings.clone(),
                    base.mesh_rules.clone(),
                )
            };
            let mut graph = render_def
                .as_ref()
                .clone()
                .into_graph(&registry, &mesh_rules)
                .expect("CodeTerminal render graph instantiates");
            let node_map = graph
                .nodes()
                .map(|node| (node.node_id.clone(), node.id))
                .collect::<Vec<_>>();
            let bindings = bindings
                .iter()
                .map(|binding| {
                    ResolvedBinding::from_static(binding, &node_map)
                        .unwrap_or_else(|| panic!("fused binding {} resolves", binding.id))
                })
                .collect();
            let bound = BoundGraph::new(bindings, &mut graph, Some(&render_def));
            let plan = compile(&graph).expect("CodeTerminal production plan compiles");
            let source_id = graph
                .instance_by_node_id(&manifold_core::NodeId::new("source"))
                .expect("source node");
            let final_id = graph
                .instance_by_node_id(&manifold_core::NodeId::new("final_output"))
                .expect("final output node");
            let source_resource = plan
                .steps()
                .iter()
                .find(|step| step.node == source_id)
                .and_then(|step| {
                    step.outputs
                        .iter()
                        .find(|(name, _)| *name == "out")
                        .map(|(_, resource)| *resource)
                })
                .expect("source output resource");
            let output_resource = plan
                .steps()
                .iter()
                .find(|step| step.node == final_id)
                .and_then(|step| {
                    step.inputs
                        .iter()
                        .find(|(name, _)| *name == "in")
                        .map(|(_, resource)| *resource)
                })
                .expect("final input resource");
            let mut backend = MetalBackend::new(std::sync::Arc::clone(&device), W, H, FMT);
            let _source_slot = backend.pre_bind_texture_2d(
                source_resource,
                RenderTarget::view_of(input.clone(), "code-terminal-input"),
            );
            let output_slot = backend.pre_bind_texture_2d(
                output_resource,
                RenderTarget::new(&device, W, H, FMT, "code-terminal-output"),
            );
            let terminal_id = graph
                .instance_by_node_id(&manifold_core::NodeId::new("terminal"))
                .expect("terminal node");
            let terminal_step = plan
                .steps()
                .iter()
                .find(|step| step.node == terminal_id)
                .expect("terminal execution step");
            let resource_for = |port: &str| {
                terminal_step
                    .outputs
                    .iter()
                    .find(|(name, _)| *name == port)
                    .map(|(_, resource)| *resource)
                    .unwrap_or_else(|| panic!("terminal output {port} resource"))
            };
            let cells_resource = resource_for("cells");
            let columns_resource = resource_for("columns");
            let rows_resource = resource_for("rows");
            // Keep observed scalar outputs alive past their final graph
            // consumer. The normal executor releases temporary bindings.
            for resource in [columns_resource, rows_resource] {
                let slot = backend.acquire(
                    resource,
                    manifold_renderer::node_graph::ports::PortType::Scalar(
                        manifold_renderer::node_graph::ports::ScalarType::F32,
                    ),
                    None,
                    (W, H),
                );
                backend.bind_resource_to_slot(resource, slot);
            }
            pre_allocate_resources(&graph, &plan, &device, &mut backend)
                .expect("CodeTerminal resources preallocate");
            Self {
                device,
                input: input.clone(),
                graph,
                plan,
                bound,
                executor: Executor::new(Box::new(backend)),
                state: StateStore::new(),
                output_slot,
                cells_resource,
                columns_resource,
                rows_resource,
            }
        }

        fn upload_source(&self, bytes: &[u8]) {
            self.device.upload_texture(&self.input, bytes);
        }

        fn terminal_cells(&self) -> (Vec<u32>, usize, usize) {
            let backend = self.executor.backend();
            let scalar = |resource| {
                let slot = backend.slot_for(resource).expect("terminal scalar slot");
                backend
                    .scalar(slot)
                    .and_then(|value| value.as_scalar())
                    .expect("terminal scalar value")
                    .round() as usize
            };
            let columns = scalar(self.columns_resource);
            let rows = scalar(self.rows_resource);
            let slot = backend
                .slot_for(self.cells_resource)
                .expect("terminal cells slot");
            let buffer = backend.array_buffer(slot).expect("terminal cells buffer");
            let count = columns.saturating_mul(rows);
            let ptr = buffer.mapped_ptr().expect("terminal cells mapped buffer");
            let values = unsafe { std::slice::from_raw_parts(ptr.cast::<u32>(), count) };
            (values.to_vec(), columns, rows)
        }

        fn render(
            &mut self,
            def: &EffectGraphDef,
            controls: Controls,
            beat: f64,
            frame: i64,
        ) -> Vec<u8> {
            let values = manifest(def, &controls.pairs());
            self.bound.apply(&mut self.graph, &values);
            let mut encoder = self.device.create_encoder("code-terminal-acceptance");
            {
                let mut gpu = GpuEncoder::new(&mut encoder, &self.device);
                self.executor.execute_frame_with_state(
                    &mut self.graph,
                    &self.plan,
                    FrameTime {
                        beats: Beats(beat),
                        seconds: Seconds(beat / 2.0),
                        delta: Seconds(1.0 / 60.0),
                        frame_count: frame,
                    },
                    &mut gpu,
                    &mut self.state,
                    0,
                );
            }
            encoder.commit_and_wait_completed();
            let texture = self
                .executor
                .backend()
                .texture_2d(self.output_slot)
                .expect("CodeTerminal output texture");
            readback_raw_halves(&self.device, texture, W, H)
        }

        fn render_with_source(
            &mut self,
            source: &[u8],
            def: &EffectGraphDef,
            controls: Controls,
            beat: f64,
            frame: i64,
        ) -> (Vec<u8>, Vec<u32>, usize, usize) {
            self.upload_source(source);
            let output = self.render(def, controls, beat, frame);
            let (cells, columns, rows) = self.terminal_cells();
            (output, cells, columns, rows)
        }

        fn write_png(&self, path: &std::path::Path) {
            let texture = self
                .executor
                .backend()
                .texture_2d(self.output_slot)
                .expect("CodeTerminal output texture");
            let rgba = readback_srgb_rgba8(&self.device, texture, W, H);
            std::fs::write(path, encode_rgba8_png(&rgba, W, H)).expect("write terminal artifact");
        }

        fn write_source_png(&self, path: &std::path::Path) {
            let rgba = readback_srgb_rgba8(&self.device, &self.input, W, H);
            std::fs::write(path, encode_rgba8_png(&rgba, W, H)).expect("write source artifact");
        }
    }

    fn f32_pixels(raw: &[u8]) -> Vec<[f32; 4]> {
        raw.chunks_exact(8)
            .map(|px| {
                [
                    f16::from_bits(u16::from_le_bytes([px[0], px[1]])).to_f32(),
                    f16::from_bits(u16::from_le_bytes([px[2], px[3]])).to_f32(),
                    f16::from_bits(u16::from_le_bytes([px[4], px[5]])).to_f32(),
                    f16::from_bits(u16::from_le_bytes([px[6], px[7]])).to_f32(),
                ]
            })
            .collect()
    }

    fn mean_abs(a: &[u8], b: &[u8]) -> f32 {
        assert_eq!(a.len(), b.len());
        a.chunks_exact(2)
            .zip(b.chunks_exact(2))
            .map(|(a, b)| {
                (f16::from_bits(u16::from_le_bytes([a[0], a[1]])).to_f32()
                    - f16::from_bits(u16::from_le_bytes([b[0], b[1]])).to_f32())
                .abs()
            })
            .sum::<f32>()
            / (a.len() / 2) as f32
    }

    fn changed_fraction(a: &[u8], b: &[u8], threshold: f32) -> f32 {
        let a = f32_pixels(a);
        let b = f32_pixels(b);
        a.iter()
            .zip(b.iter())
            .filter(|(a, b)| {
                a.iter()
                    .zip(b.iter())
                    .take(3)
                    .map(|(a, b)| (a - b).abs())
                    .fold(0.0, f32::max)
                    > threshold
            })
            .count() as f32
            / a.len() as f32
    }

    fn max_alpha_error(a: &[u8], b: &[u8]) -> f32 {
        f32_pixels(a)
            .iter()
            .zip(f32_pixels(b).iter())
            .map(|(a, b)| (a[3] - b[3]).abs())
            .fold(0.0, f32::max)
    }

    fn bright_pixel_count(raw: &[u8]) -> usize {
        raw.chunks_exact(8)
            .filter(|px| f16::from_bits(u16::from_le_bytes([px[0], px[1]])).to_f32() > 0.9)
            .count()
    }

    fn changed_rows(a: &[u32], b: &[u32], columns: usize, rows: usize) -> Vec<usize> {
        (0..rows)
            .filter(|row| {
                let start = row * columns;
                a[start..start + columns] != b[start..start + columns]
            })
            .collect()
    }

    fn assert_terminal_cells(cells: &[u32], columns: usize) {
        assert!(
            cells.iter().all(|cell| (32..=127).contains(cell)),
            "terminal cells stay printable ASCII or cursor code"
        );
        assert!(
            cells
                .chunks(columns)
                .all(|row| row.chunks(32).all(|segment| segment
                    .iter()
                    .filter(|&&cell| cell == 127)
                    .count()
                    <= 1)),
            "each independently typing row segment has at most one cursor"
        );
    }

    #[test]
    fn code_terminal_bounded_acceptance_and_fusion_proof() {
        let device = std::sync::Arc::new(GpuDevice::new());
        let def = preset();
        let (input, source_raw) = fixture(&device);
        let mut unfused = Harness::new(std::sync::Arc::clone(&device), &def, &input, false);

        let source = unfused.render(
            &def,
            Controls {
                erosion: 0.0,
                ..Controls::defaults()
            },
            0.0,
            0,
        );
        assert!(
            mean_abs(&source, &source_raw) < 0.0001,
            "erosion=0 preserves source identity"
        );

        let full = unfused.render(&def, Controls::defaults(), 0.0, 1);
        assert!(
            changed_fraction(&source_raw, &full, 0.02) > 0.05,
            "full erosion shows glyph coverage"
        );
        let full_pixels = f32_pixels(&full);
        let dark = full_pixels.iter().filter(|p| p[1] < 0.002).count();
        let lit = full_pixels.iter().filter(|p| p[1] > 0.08).count();
        assert!(
            dark > full_pixels.len() / 3 && lit > full_pixels.len() / 100,
            "terminal must contain both black cell backgrounds and visible glyph ink"
        );
        assert!(
            max_alpha_error(&source_raw, &full) < 0.001,
            "full erosion preserves source alpha"
        );

        let mid = unfused.render(
            &def,
            Controls {
                erosion: 0.5,
                ..Controls::defaults()
            },
            0.0,
            2,
        );
        let mid_changed = changed_fraction(&source_raw, &mid, 0.02);
        assert!(
            (0.02..0.98).contains(&mid_changed),
            "intermediate erosion keeps both source and terminal regions: {mid_changed:.3}"
        );

        let frozen_a = unfused.render(
            &def,
            Controls {
                activity: 0.0,
                ..Controls::defaults()
            },
            1.0,
            3,
        );
        let frozen_b = unfused.render(
            &def,
            Controls {
                activity: 0.0,
                ..Controls::defaults()
            },
            4.0,
            4,
        );
        assert!(
            mean_abs(&frozen_a, &frozen_b) < 0.001,
            "Activity=0 freezes over beat time"
        );
        let active_a = unfused.render(&def, Controls::defaults(), 5.0, 5);
        let active_b = unfused.render(&def, Controls::defaults(), 6.5, 6);
        assert!(
            mean_abs(&active_a, &active_b) > 0.0005,
            "Activity=1 advances typing/scroll"
        );

        // The reaction input is deliberately two equal-area, equal-brightness
        // rectangles. Their spatial separation must reach the cell buffer;
        // comparing only the final image would allow a static text mask to
        // pass this acceptance check.
        let source_a = source_frame(64, H / 5);
        let source_b = source_frame(W - RECT_W - 64, H / 5);
        assert_eq!(
            bright_pixel_count(&source_a),
            bright_pixel_count(&source_b),
            "spatial source fixtures have equal total brightness"
        );
        let (input_a, _) = fixture(&device);
        let (input_b, _) = fixture(&device);
        let mut moved_a = Harness::new(std::sync::Arc::clone(&device), &def, &input_a, false);
        let mut moved_b = Harness::new(std::sync::Arc::clone(&device), &def, &input_b, false);
        let (_, _, columns_a, rows_a) =
            moved_a.render_with_source(&source_a, &def, Controls::defaults(), 0.0, 0);
        let (_, cells_a, _, _) =
            moved_a.render_with_source(&source_a, &def, Controls::defaults(), 0.25, 1);
        let (_, _, columns_b, rows_b) =
            moved_b.render_with_source(&source_b, &def, Controls::defaults(), 0.0, 0);
        let (_, cells_b, _, _) =
            moved_b.render_with_source(&source_b, &def, Controls::defaults(), 0.25, 1);
        assert_eq!((columns_a, rows_a), (columns_b, rows_b));
        assert_terminal_cells(&cells_a, columns_a);
        assert_terminal_cells(&cells_b, columns_b);
        let source_rows = source_affected_rows(H / 5, rows_a);
        let spatially_changed = changed_rows(&cells_a, &cells_b, columns_a, rows_a);
        assert!(
            spatially_changed
                .iter()
                .any(|row| source_rows.contains(row)),
            "source motion changes terminal characters in source-affected rows"
        );

        // Activity 0 is a cell-state freeze, even while the reaction image
        // moves. Resuming activity must make the previously frozen cells live.
        let (frozen_input, _) = fixture(&device);
        let mut frozen = Harness::new(std::sync::Arc::clone(&device), &def, &frozen_input, false);
        let (_, frozen_a, columns, _) = frozen.render_with_source(
            &source_a,
            &def,
            Controls {
                activity: 0.0,
                ..Controls::defaults()
            },
            0.0,
            0,
        );
        let (_, frozen_b, _, _) = frozen.render_with_source(
            &source_b,
            &def,
            Controls {
                activity: 0.0,
                ..Controls::defaults()
            },
            4.0,
            1,
        );
        assert_eq!(
            frozen_a, frozen_b,
            "Activity=0 freezes actual terminal cells"
        );
        let (_, resumed, _, _) =
            frozen.render_with_source(&source_b, &def, Controls::defaults(), 5.0, 2);
        assert_ne!(
            frozen_b, resumed,
            "resuming activity reacts to the moving source"
        );
        assert_terminal_cells(&resumed, columns);

        // A static source still animates the pane history beyond the live
        // bottom row; this catches tests that only inspect the current cursor.
        let (animated_input, _) = fixture(&device);
        let mut animated =
            Harness::new(std::sync::Arc::clone(&device), &def, &animated_input, false);
        let (_, first_cells, columns, rows) =
            animated.render_with_source(&source_a, &def, Controls::defaults(), 0.0, 0);
        let (_, later_cells, _, _) =
            animated.render_with_source(&source_a, &def, Controls::defaults(), 8.0, 1);
        assert!(
            changed_rows(&first_cells, &later_cells, columns, rows)
                .iter()
                .any(|row| *row + 1 < rows),
            "static source animates terminal rows above the live bottom row"
        );

        let small = unfused.render(
            &def,
            Controls {
                text_size: 8.0,
                ..Controls::defaults()
            },
            0.0,
            7,
        );
        let large = unfused.render(
            &def,
            Controls {
                text_size: 48.0,
                ..Controls::defaults()
            },
            0.0,
            8,
        );
        assert!(
            mean_abs(&small, &large) > 0.001,
            "Text Size 8 vs 48 changes output"
        );
        let shadows = unfused.render(
            &def,
            Controls {
                erosion: 0.5,
                tonal_bias: -1.0,
                ..Controls::defaults()
            },
            0.0,
            9,
        );
        let highlights = unfused.render(
            &def,
            Controls {
                erosion: 0.5,
                tonal_bias: 1.0,
                ..Controls::defaults()
            },
            0.0,
            10,
        );
        assert!(
            mean_abs(&shadows, &highlights) > 0.001,
            "Tonal Bias selects different erosion tones"
        );
        let source_colour = unfused.render(
            &def,
            Controls {
                colour: 0.0,
                ..Controls::defaults()
            },
            0.0,
            11,
        );
        let green = unfused.render(
            &def,
            Controls {
                colour: 1.0,
                ..Controls::defaults()
            },
            0.0,
            12,
        );
        let amber = unfused.render(
            &def,
            Controls {
                colour: 2.0,
                ..Controls::defaults()
            },
            0.0,
            13,
        );
        assert!(
            mean_abs(&source_colour, &green) > 0.001,
            "Source and Green palettes differ"
        );
        assert!(
            mean_abs(&green, &amber) > 0.001,
            "Green and Amber palettes differ"
        );

        let base = loaded_preset_view_by_id(&manifold_core::PresetTypeId::new("CodeTerminal"))
            .expect("CodeTerminal loaded view");
        let fused_view = fused_view_for(&def, base).expect("CodeTerminal has a fused view");
        let fused_nodes = fused_view
            .canonical_def
            .nodes
            .iter()
            .filter(|node| node.type_id == "node.wgsl_compute")
            .count();
        assert!(fused_nodes > 0, "fused preset contains actual fused nodes");
        let mut fusion_unfused = Harness::new(std::sync::Arc::clone(&device), &def, &input, false);
        let mut fused = Harness::new(std::sync::Arc::clone(&device), &def, &input, true);
        let mut unfused_warm = Vec::new();
        let mut fused_warm = Vec::new();
        for (frame, beat) in [(0_i64, 0.0), (1, 0.25), (2, 0.5)] {
            let source = source_frame(32 + frame as u32 * 96, H / 5);
            // Both harnesses point at the same source texture. Upload once
            // before the frame so their fenced analysis sees the same image
            // and beat timeline.
            fusion_unfused.upload_source(&source);
            unfused_warm = fusion_unfused.render(&def, Controls::defaults(), beat, frame);
            fused_warm = fused.render(&def, Controls::defaults(), beat, frame);
        }
        assert!(
            mean_abs(&unfused_warm, &fused_warm) < 0.001,
            "fused and unfused CodeTerminal outputs agree after warmup"
        );

        if let Ok(dir) = std::env::var("MANIFOLD_TERMINAL_DEMO_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("create terminal demo dir");
            let mut demo = Harness::new(std::sync::Arc::clone(&device), &def, &input, false);
            // Warm the fenced source readback before selecting evenly spaced
            // frames from one continuous moving-silhouette capture.
            for frame in 0_u32..4 {
                let x = frame * (W - RECT_W) / 8;
                let y = H / 5 + frame * (H - RECT_H - H / 5) / 8;
                let source = source_frame(x, y);
                demo.render_with_source(
                    &source,
                    &def,
                    Controls {
                        text_size: 32.0,
                        ..Controls::defaults()
                    },
                    f64::from(frame) * 0.25,
                    i64::from(frame),
                );
            }
            // One continuous two-second capture at 32 fps: retain every
            // frame so character-by-character typing can be reviewed.
            for frame in 0_u32..64 {
                let x = frame * (W - RECT_W) / 63;
                let y = H / 4;
                let source = source_frame(x, y);
                demo.render_with_source(
                    &source,
                    &def,
                    Controls {
                        text_size: 32.0,
                        ..Controls::defaults()
                    },
                    1.0 + f64::from(frame) / 16.0,
                    i64::from(frame) + 4,
                );
                demo.write_png(&dir.join(format!("code-terminal-frame-{frame:02}.png")));
                demo.write_source_png(&dir.join(format!("source-frame-{frame:02}.png")));
            }
        }
    }
}
