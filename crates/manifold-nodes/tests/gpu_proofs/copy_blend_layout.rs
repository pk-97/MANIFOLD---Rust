//! Copy Blend's target is the copies' own arrangement with only Layout
//! switched: copy `i` heads for the slot copy `i` takes in that layout
//! (`manifold_core::copy_layout`), so no two blend paths cross. A cube's
//! nine copies lie on a Line; Copy Blend aims them at a Grid of the same
//! Columns and Spacing. `COPY_BLEND_PNG_DIR=dir` saves the mid-blend still.

use manifold_core::copy_layout::{Arrangement, CopyLayout};
use manifold_core::layer::Layer;
use manifold_core::scene_doc::{
    Item, ItemFields, ItemId, ItemKind, ModifierFields, ModifierTargets, ModifierType, SceneDoc,
};
use manifold_core::{PresetTypeId, Seconds};
use manifold_gpu::BufferUse;
use manifold_gpu::{GpuBuffer, GpuTextureFormat};
use manifold_node_engine::exec::effect_node::{NodeInstanceId, SimStep};
use manifold_node_engine::exec::execution_plan::ResourceId;
use manifold_node_engine::gpu::gpu_encoder::GpuEncoder;
use manifold_node_engine::gpu::headless_readback::readback_to_srgb_png;
use manifold_node_engine::gpu::render_target::RenderTarget;
use manifold_node_engine::mesh::InstanceTransform;
use manifold_node_engine::persistence::PrimitiveRegistry;
use manifold_node_engine::runtime::PresetRuntime;
use manifold_node_engine::runtime::preset_context::PresetContext;

const WIDTH: u32 = 640;
const HEIGHT: u32 = 360;
const COUNT: usize = 9;
const COLUMNS: usize = 3;

fn scene() -> SceneDoc {
    let mut doc = SceneDoc::base();
    if let Some(Item {
        fields: ItemFields::Object(cube),
        ..
    }) = doc.find_mut(&ItemId::new("cube"))
    {
        cube.copies = true;
    }
    let mut card = ItemFields::default_for(ItemKind::Modifier);
    if let ItemFields::Modifier(fields) = &mut card {
        *fields = ModifierFields {
            modifier: ModifierType::CopyBlend,
            on: true,
            targets: ModifierTargets::Items(vec![ItemId::new("cube")]),
            ..fields.clone()
        };
    }
    let end = doc.items.len();
    doc.insert(None, end, Item::new(ItemId::new("blend"), "blend", card))
        .unwrap();
    doc
}

struct Run {
    device: std::sync::Arc<manifold_gpu::GpuDevice>,
    runtime: PresetRuntime,
    target: RenderTarget,
}

impl Run {
    /// The copies on a Line, blended `mix` of the way to a Grid.
    fn new(mix: f32) -> Self {
        let mut layer =
            Layer::new_generator("Scene".into(), PresetTypeId::from_string("Scene".into()), 0);
        layer
            .gen_params_mut()
            .unwrap()
            .install_scene_document(scene())
            .unwrap();
        let mut fx = layer.gen_params().unwrap().clone();
        for (id, value) in [
            ("cube.count", COUNT as f32),
            ("cube.columns", COLUMNS as f32),
            ("cube.layout", CopyLayout::Line as u32 as f32),
            ("blend.layout", CopyLayout::Grid as u32 as f32),
            ("blend.mix", mix),
            ("camera.distance", 16.0),
        ] {
            assert!(fx.set_base_param(id, value), "the scene has a {id} row");
        }
        let device =
            std::sync::Arc::clone(&manifold_node_engine::testkit::gpu_harness::shared().device);
        let registry = PrimitiveRegistry::with_builtin();
        let mut runtime = PresetRuntime::from_def_for_render(
            fx.graph.clone().unwrap(),
            &registry,
            Some(&fx.params),
            false,
        )
        .expect("the scene builds")
        .with_generator_device(
            std::sync::Arc::clone(&device),
            WIDTH,
            HEIGHT,
            GpuTextureFormat::Rgba16Float,
        )
        .expect("the scene installs on the device");
        runtime.apply_param_values(&fx.params);
        runtime.set_dump_all(true);
        let target = RenderTarget::new(
            &device,
            WIDTH,
            HEIGHT,
            GpuTextureFormat::Rgba16Float,
            "copy-blend-layout",
        );
        let mut run = Self {
            device,
            runtime,
            target,
        };
        for frame in 0..4 {
            run.frame(frame, &fx.params);
        }
        run
    }

    fn frame(&mut self, frame: i64, params: &manifold_core::params::ParamManifest) {
        let context = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: WIDTH,
            height: HEIGHT,
            output_width: WIDTH,
            output_height: HEIGHT,
            aspect: WIDTH as f32 / HEIGHT as f32,
            owner_key: 0,
            is_clip_level: false,
            frame_count: frame,
            anim_progress: 0.0,
            trigger_count: 0,
        };
        self.runtime.set_dump_all(true);
        self.runtime
            .set_sim_step(SimStep::live(Seconds(1.0 / 60.0)));
        let mut encoder = self.device.create_encoder("copy-blend-layout");
        self.runtime.render(
            &mut GpuEncoder::new(&mut encoder, &self.device),
            &self.target.texture,
            &context,
            params,
        );
        encoder.commit_and_wait().expect("GPU work completes");
    }

    fn type_of(&self, node: NodeInstanceId) -> String {
        self.runtime
            .graph
            .get_node(node)
            .map(|n| n.node.type_id().as_str().to_string())
            .unwrap_or_default()
    }

    /// The resource on the blend's `port`: `a` the copies, `b` the target,
    /// `out` the blend.
    fn blend(&self, port: &str) -> ResourceId {
        let step = self
            .runtime
            .plan
            .steps()
            .iter()
            .find(|s| self.type_of(s.node) == "node.blend_copies")
            .expect("a Copy Blend step");
        step.inputs
            .iter()
            .chain(&step.outputs)
            .find(|(p, _)| *p == port)
            .map(|&(_, r)| r)
            .expect("blend port")
    }

    fn read(&self, res: ResourceId) -> Vec<InstanceTransform> {
        let backend = self.runtime.backend_for_test();
        let buffer: &GpuBuffer = backend
            .array_buffer(backend.slot_for(res).expect("resource bound"))
            .expect("array resource");
        let bytes = (COUNT * std::mem::size_of::<InstanceTransform>()) as u64;
        let shared = self.device.create_buffer(bytes, BufferUse::Upload);
        let mut encoder = self.device.create_encoder("copy-blend-readback");
        encoder.copy_buffer_to_buffer(buffer, &shared, bytes);
        encoder.commit_and_wait().expect("GPU work completes");
        let ptr = shared.mapped_ptr().expect("shared readback");
        unsafe { std::slice::from_raw_parts(ptr as *const InstanceTransform, COUNT) }.to_vec()
    }
}

fn close(a: [f32; 4], b: [f32; 4]) -> bool {
    (0..4).all(|k| (a[k] - b[k]).abs() < 1.0e-4)
}

#[test]
fn copy_blend_sends_copy_i_to_slot_i_of_the_copies_own_layout() {
    let run = Run::new(0.5);
    let copies = run.read(run.blend("a"));
    let target = run.read(run.blend("b"));
    let blended = run.read(run.blend("out"));

    // The copies' own Line: copy i is i steps along X from the first.
    let origin = [
        copies[0].pos_scale[0],
        copies[0].pos_scale[1],
        copies[0].pos_scale[2],
    ];
    let step = copies[1].pos_scale[0] - copies[0].pos_scale[0];
    assert!(step > 0.0, "the copies lie on a Line, step {step}");
    let spacing = manifold_core::copies_body::SIZE_SPACING_DEFAULT;
    let width = step / spacing;
    let grid = Arrangement {
        layout: CopyLayout::Grid,
        count: COUNT,
        columns: COLUMNS,
        spacing,
        turn: [0.0; 3],
        origin,
        rotation: [0.0; 3],
    };
    for i in 0..COUNT {
        let slot = grid.rest([width; 3], i).position;
        let expected = [slot[0], slot[1], slot[2], copies[i].pos_scale[3]];
        assert!(
            close(target[i].pos_scale, expected),
            "copy {i} aims at Grid slot {i} {expected:?}, got {:?}",
            target[i].pos_scale
        );
        let midway: [f32; 4] =
            std::array::from_fn(|k| 0.5 * (copies[i].pos_scale[k] + target[i].pos_scale[k]));
        assert!(
            close(blended[i].pos_scale, midway),
            "copy {i} sits halfway at Mix 0.5: {midway:?}, got {:?}",
            blended[i].pos_scale
        );
    }

    let png = readback_to_srgb_png(&run.device, &run.target.texture, WIDTH, HEIGHT)
        .expect("GPU readback completes");
    if let Ok(dir) = std::env::var("COPY_BLEND_PNG_DIR") {
        std::fs::write(std::path::Path::new(&dir).join("copy_blend_mid.png"), png).unwrap();
    }
}
