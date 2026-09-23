//! Bounded acceptance coverage for the Code Terminal effect.
//!
//! The CPU gate checks the on-disk contract and exercises the shared binding
//! path.  The GPU gate renders one 960×540 fixture through the production
//! graph executor. It samples bounded source timelines and layout choices
//! instead of becoming a thumbnail/render sweep.

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
            ("layout", 2.0),
        ],
    );
    bound.apply(&mut graph, &values);
    assert_eq!(bound.bindings.len(), 6, "every card control has a route");
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
    assert_eq!(
        graph
            .get_node(
                graph
                    .instance_by_node_id(&manifold_core::NodeId::new("terminal"))
                    .expect("terminal node"),
            )
            .expect("terminal node live")
            .params
            .get("layout"),
        Some(&manifold_renderer::node_graph::ParamValue::Enum(2)),
        "terminal.layout binding"
    );

    let layout = roundtrip
        .preset_metadata
        .as_ref()
        .expect("CodeTerminal metadata")
        .params
        .iter()
        .find(|spec| spec.id == "layout")
        .expect("layout card control");
    assert_eq!(
        (layout.min, layout.max, layout.default_value),
        (0.0, 3.0, 0.0)
    );
    assert!(layout.whole_numbers, "layout selects whole-number modes");
    assert_eq!(
        layout
            .value_labels
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["Single", "Vertical Split", "Horizontal Split", "Four Panes"]
    );

    // A v2 graph saved before the layout control existed must still select
    // the terminal primitive's Single-pane default when loaded today.
    let mut legacy = roundtrip.clone();
    if let Some(metadata) = legacy.preset_metadata.as_mut() {
        metadata.params.retain(|spec| spec.id != "layout");
        metadata.bindings.retain(|binding| binding.id != "layout");
    }
    legacy
        .nodes
        .iter_mut()
        .find(|node| node.node_id == manifold_core::NodeId::new("terminal"))
        .expect("terminal node")
        .params
        .remove("layout");
    let (mut legacy_graph, mut legacy_bound) = binding_graph(&legacy, &registry);
    compile(&legacy_graph).expect("legacy CodeTerminal graph compiles");
    legacy_bound.apply(&mut legacy_graph, &manifest(&legacy, &[]));
    let terminal = legacy_graph
        .get_node(
            legacy_graph
                .instance_by_node_id(&manifold_core::NodeId::new("terminal"))
                .expect("legacy terminal node"),
        )
        .expect("legacy terminal node live");
    assert_eq!(
        terminal.params.get("layout"),
        Some(&manifold_renderer::node_graph::ParamValue::Enum(0)),
        "absent layout defaults to Single"
    );
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
        layout: f32,
    }

    impl Controls {
        fn defaults() -> Self {
            Self {
                erosion: 1.0,
                text_size: 24.0,
                activity: 1.0,
                tonal_bias: 0.0,
                colour: 1.0,
                layout: 0.0,
            }
        }

        fn pairs(self) -> [(&'static str, f32); 6] {
            [
                ("erosion", self.erosion),
                ("text_size", self.text_size),
                ("activity", self.activity),
                ("tonal_bias", self.tonal_bias),
                ("colour", self.colour),
                ("layout", self.layout),
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

    // Demo-only curved silhouette: observe whether row contours shape text
    // naturally while existing rectangular fixtures retain their proof scope.
    fn curved_source_frame(center: f32) -> Vec<u8> {
        let mut bytes = Vec::with_capacity((W * H * 8) as usize);
        for y in 0..H {
            for x in 0..W {
                let dx = (x as f32 / W as f32 - center) / 0.25;
                let dy = (y as f32 / H as f32 - 0.5) / 0.4;
                let coverage = ((1.0 - dx * dx - dy * dy) * 12.0).clamp(0.0, 1.0);
                let value = 0.02 + 0.98 * coverage;
                for channel in [value, value, value, 1.0] {
                    bytes.extend_from_slice(&f16::from_f32(channel).to_le_bytes());
                }
            }
        }
        bytes
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
            cells.iter().filter(|&&cell| cell == 127).count() <= 3,
            "source-driven edits keep at most three active cursors"
        );
        assert_eq!(cells.len() % columns, 0, "complete terminal rows");
    }

    fn assert_meaningful_long_line(cells: &[u32], columns: usize) {
        let longest = cells
            .chunks(columns)
            .map(|row| {
                row.iter()
                    .filter(|&&cell| (33..=126).contains(&cell))
                    .count()
            })
            .max()
            .unwrap_or(0);
        assert!(
            longest >= 16,
            "terminal source produces a meaningful long line (longest={longest})"
        );
    }

    fn assert_most_rows_stable(previous: &[u32], current: &[u32], columns: usize, rows: usize) {
        let changed = changed_rows(previous, current, columns, rows).len();
        assert!(
            changed <= 4,
            "only changed source spans edit rows between adjacent frames (changed={changed})"
        );
    }

    fn assert_tmux_borders(cells: &[u32]) {
        for border in [b'+', b'-', b'|'] {
            assert!(
                cells.contains(&u32::from(border)),
                "tmux layout contains ASCII '{}' border",
                char::from(border)
            );
        }
    }

    fn settle_source(
        harness: &mut Harness,
        source: &[u8],
        def: &EffectGraphDef,
        controls: Controls,
        label: &str,
    ) -> (Vec<u32>, usize, usize) {
        settle_source_from(harness, source, def, controls, label, 0)
    }

    fn settle_source_from(
        harness: &mut Harness,
        source: &[u8],
        def: &EffectGraphDef,
        controls: Controls,
        label: &str,
        start_frame: i64,
    ) -> (Vec<u32>, usize, usize) {
        let mut previous: Option<Vec<u32>> = None;
        let mut stable_frames = 0;
        let mut settled = None;
        for offset in 0_i64..64 {
            let frame = start_frame + offset;
            let (_, cells, columns, rows) =
                harness.render_with_source(source, def, controls, frame as f64 * 0.25, frame);
            assert_terminal_cells(&cells, columns);
            if let Some(previous) = previous.as_ref() {
                if frame >= 8 {
                    assert_most_rows_stable(previous, &cells, columns, rows);
                }
                if previous == &cells {
                    stable_frames += 1;
                } else {
                    stable_frames = 0;
                }
            }
            previous = Some(cells.clone());
            if stable_frames >= 8 {
                settled = Some((cells, columns, rows));
                break;
            }
        }
        settled.unwrap_or_else(|| panic!("{label} did not settle within 64 quarter-beat frames"))
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
        let mut moved = Harness::new(std::sync::Arc::clone(&device), &def, &input_a, false);
        let (cells_a, columns_a, rows_a) = settle_source(
            &mut moved,
            &source_a,
            &def,
            Controls::defaults(),
            "source A",
        );
        let (cells_b, columns_b, rows_b) = settle_source_from(
            &mut moved,
            &source_b,
            &def,
            Controls::defaults(),
            "source B",
            64,
        );
        assert_eq!((columns_a, rows_a), (columns_b, rows_b));
        assert_terminal_cells(&cells_a, columns_a);
        assert_terminal_cells(&cells_b, columns_b);
        assert_meaningful_long_line(&cells_a, columns_a);
        assert_meaningful_long_line(&cells_b, columns_b);
        let source_rows = source_affected_rows(H / 5, rows_a);
        let spatially_changed = changed_rows(&cells_a, &cells_b, columns_a, rows_a);
        assert!(
            spatially_changed
                .iter()
                .any(|row| source_rows.contains(row)),
            "source motion changes terminal characters in source-affected rows"
        );

        // A settled stationary source is idempotent over advancing beats. The
        // bounded helper also checks that source edits touch only a few rows
        // while the shared cell buffer is settling.
        let (static_input, _) = fixture(&device);
        let mut stationary =
            Harness::new(std::sync::Arc::clone(&device), &def, &static_input, false);
        let (stationary_cells, stationary_columns, _) = settle_source(
            &mut stationary,
            &source_a,
            &def,
            Controls::defaults(),
            "stationary source",
        );
        let mut repeated = stationary_cells.clone();
        for frame in 64_i64..72 {
            let (_, cells, columns, rows) = stationary.render_with_source(
                &source_a,
                &def,
                Controls::defaults(),
                frame as f64 * 0.25,
                frame,
            );
            assert_eq!(columns, stationary_columns);
            assert_terminal_cells(&cells, columns);
            assert_eq!(
                cells, repeated,
                "static source remains stable at beat {frame}"
            );
            assert_most_rows_stable(&repeated, &cells, columns, rows);
            repeated = cells;
        }

        // Each tmux layout keeps the same external grid contract while
        // producing its own pane arrangement and ASCII border.
        let mut layouts = Vec::new();
        for layout_index in 0..=3 {
            let layout = layout_index as f32;
            let (layout_input, _) = fixture(&device);
            let mut harness =
                Harness::new(std::sync::Arc::clone(&device), &def, &layout_input, false);
            let controls = Controls {
                layout,
                ..Controls::defaults()
            };
            let (cells, columns, rows) = settle_source(
                &mut harness,
                &source_a,
                &def,
                controls,
                &format!("layout {layout}"),
            );
            assert_terminal_cells(&cells, columns);
            if layout > 0.0 {
                assert_tmux_borders(&cells);
            }
            assert_meaningful_long_line(&cells, columns);
            if layout == 3.0 {
                for label in [b"0: shell".as_slice(), b"1: code", b"2: logs", b"3: inspect"] {
                    assert!(
                        cells.windows(label.len()).any(|window| window
                            .iter()
                            .zip(label)
                            .all(|(&cell, &byte)| cell == u32::from(byte))),
                        "four panes retain their role labels"
                    );
                }
            }
            if layout == 0.0 {
                assert!(
                    cells.chunks(columns).any(|row| row[64..]
                        .iter()
                        .filter(|&&c| (33..=126).contains(&c))
                        .count()
                        >= 12),
                    "Single has statements extending beyond the old repeated columns"
                );
            }
            layouts.push((cells, columns, rows));
        }
        for left in 0..layouts.len() {
            for right in left + 1..layouts.len() {
                assert_ne!(
                    layouts[left].0, layouts[right].0,
                    "tmux layouts {left} and {right} have distinct cell arrangements"
                );
            }
        }
        assert!(
            layouts
                .iter()
                .all(|(_, columns, rows)| (*columns, *rows) == (columns_a, rows_a)),
            "layout selection preserves the external cells/grid contract"
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
        let mut resumed = frozen_b.clone();
        for frame in 1_i64..=64 {
            let (_, cells, _, _) = frozen.render_with_source(
                &source_b,
                &def,
                Controls::defaults(),
                4.0 + frame as f64 * 0.25,
                frame + 1,
            );
            if cells != frozen_b {
                resumed = cells;
                break;
            }
        }
        assert_ne!(
            frozen_b, resumed,
            "resuming activity reacts to the moving source"
        );
        assert_terminal_cells(&resumed, columns);

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
        let (unfused_cells, unfused_columns, unfused_rows) = fusion_unfused.terminal_cells();
        let (fused_cells, fused_columns, fused_rows) = fused.terminal_cells();
        assert_eq!(
            (unfused_columns, unfused_rows),
            (fused_columns, fused_rows),
            "fused and unfused terminal grids agree"
        );
        assert_eq!(
            unfused_cells, fused_cells,
            "fused and unfused terminal cells agree on the same source timeline"
        );

        if let Ok(dir) = std::env::var("MANIFOLD_TERMINAL_DEMO_DIR") {
            let dir = std::path::PathBuf::from(dir);
            std::fs::create_dir_all(&dir).expect("create terminal demo dir");
            // Keep one continuous 64-frame / 32-fps moving-source sequence
            // for each layout, then capture its settled static endpoint.
            for (layout, name) in [(0.0_f32, "single"), (3.0_f32, "four-panes")] {
                let (demo_input, _) = fixture(&device);
                let mut demo =
                    Harness::new(std::sync::Arc::clone(&device), &def, &demo_input, false);
                let controls = Controls {
                    text_size: 32.0,
                    layout,
                    ..Controls::defaults()
                };
                for frame in 0_u32..64 {
                    let source = curved_source_frame(0.25 + 0.5 * frame as f32 / 63.0);
                    demo.render_with_source(
                        &source,
                        &def,
                        controls,
                        f64::from(frame) / 16.0,
                        i64::from(frame),
                    );
                    demo.write_png(&dir.join(format!("code-terminal-{name}-frame-{frame:02}.png")));
                    demo.write_source_png(&dir.join(format!("source-{name}-frame-{frame:02}.png")));
                }
                let settled_source = curved_source_frame(0.75);
                settle_source_from(
                    &mut demo,
                    &settled_source,
                    &def,
                    controls,
                    &format!("{name} demo endpoint"),
                    64,
                );
                demo.write_png(&dir.join(format!("code-terminal-{name}-settled.png")));
            }
        }
    }
}
