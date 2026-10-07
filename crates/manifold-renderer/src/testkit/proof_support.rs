use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::{ExecutionPlan, ResourceId, compile};
use crate::node_graph::Graph;
use crate::node_graph::ParamValue;
use crate::node_graph::primitives::Gain;
use crate::node_graph::{
    EffectGraphDefExt, Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId,
    PrimitiveRegistry, Source, StateStore,
};
use crate::render_target::RenderTarget;
use half::f16;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;





pub(crate) fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

pub(crate) fn find_node(graph: &Graph, type_id: &str) -> NodeInstanceId {
    graph
        .nodes()
        .find(|n| n.node.type_id().as_str() == type_id)
        .map(|n| n.id)
        .unwrap_or_else(|| panic!("ColorGrade graph missing a `{type_id}` node"))
}

pub(crate) fn set_f(graph: &mut Graph, type_id: &str, param: &str, v: f32) {
    let id = find_node(graph, type_id);
    graph
        .set_param(id, param, ParamValue::Float(v))
        .unwrap_or_else(|e| panic!("set {type_id}.{param}: {e:?}"));
}

pub(crate) fn resource_for_output(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?}");
}

pub(crate) fn try_resource_for_output(
    plan: &ExecutionPlan,
    node: NodeInstanceId,
    port: &str,
) -> Option<ResourceId> {
    plan.steps()
        .iter()
        .find(|step| step.node == node)
        .and_then(|step| {
            step.outputs
                .iter()
                .find(|(name, _)| *name == port)
                .map(|(_, resource)| *resource)
        })
}

/// CPU-built RGBA gradient as a CPU-uploadable source texture — spatially
/// varying so a pointwise fusion bug that's invisible on a flat fill can't
/// hide. R ramps in x, G in y, B fixed, A = 1.
pub(crate) fn gradient_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(1.0);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "freeze-proof-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Render an effect graph to a standalone texture (the unfused / oracle side).
/// Copies `input` into the source slot, runs one frame, copies the bound
/// output into a fresh target that outlives the backend.
pub(crate) fn render_graph(
    device: &std::sync::Arc<GpuDevice>,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, w, h, FMT, "freeze-graph-result");
    let out_tex = exec
        .backend()
        .texture_2d(out_slot)
        .expect("graph output texture retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

/// Like [`render_graph`], but run the graph at a specific [`FrameTime`] so
/// tests can prove a fused kernel is NOT freezing its frame-derived inputs.
pub(crate) fn render_graph_at_time(
    device: &std::sync::Arc<GpuDevice>,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
    ft: FrameTime,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    // StateStore-aware dispatch: defs carrying a stateful node (Watercolor's
    // temporal feedback) refuse the plain execute path.
    let mut state = StateStore::new();
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_state(graph, plan, ft, &mut gpu, &mut state, 0);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, w, h, FMT, "freeze-graph-result");
    let out_tex = exec
        .backend()
        .texture_2d(out_slot)
        .expect("graph output texture retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

/// Render the hand-fused Gain kernel: `out.rgb = in.rgb * product`, alpha kept.
/// One read, one multiply, one write — the bandwidth collapse of an N-Gain
/// chain.
pub(crate) fn render_fused_gain(device: &GpuDevice, input: &GpuTexture, product: f32) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(
        include_str!("../node_graph/freeze/shaders/gain_fused.wgsl"),
        "cs_main",
        "freeze.gain_fused",
    );
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-fused-out");
    let u = FusedGainU {
        product,
        _pad: [0.0; 3],
    };
    let mut enc = device.create_encoder("freeze-fused-exec");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&u),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: input,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: &out_rt.texture,
            },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "freeze.gain_fused",
    );
    enc.commit_and_wait_completed();
    out_rt
}

/// Build the unfused `Source -> Gain(g1) -> Gain(g2) -> FinalOutput` chain and
/// render it. Returns (rendered texture, the source ResourceId is internal).
pub(crate) fn render_unfused_two_gain(device: &std::sync::Arc<GpuDevice>, input: &GpuTexture, g1: f32, g2: f32) -> RenderTarget {
    let mut g = Graph::new();
    let src = g.add_node(Box::new(Source::new()));
    let a = g.add_node(Box::new(Gain::new()));
    let b = g.add_node(Box::new(Gain::new()));
    let fout = g.add_node(Box::new(FinalOutput::new()));
    g.set_param(a, "gain", ParamValue::Float(g1)).unwrap();
    g.set_param(b, "gain", ParamValue::Float(g2)).unwrap();
    g.connect((src, "out"), (a, "in")).unwrap();
    g.connect((a, "out"), (b, "in")).unwrap();
    g.connect((b, "out"), (fout, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let source_res = resource_for_output(&plan, src, "out");
    let output_res = resource_for_output(&plan, b, "out");
    render_graph(device, &mut g, &plan, source_res, input, output_res)
}

/// CPU-built gradient with spatially-varying alpha (A ramps in x), so the
/// faithful per-atom alpha threading (mix lerps a.a→b.a in Lerp mode, and
/// passes a.a through untouched in every other mode — BUG-181) is observable
/// in the diff — the section 12.4 hardened-fixture alpha axis. R/G/B as in
/// `gradient_input`.
pub(crate) fn gradient_input_varying_alpha(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(0.25 + 0.7 * (x as f32 / w as f32));
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "freeze-proof-input-alpha",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Deterministic LCG (Numerical Recipes constants) — a fuzzer needs random
/// coverage but a *reproducible* seed so a failure can be replayed exactly
/// (design section 12.3 step 7 reproducer). Not for crypto; just spreads samples.
pub(crate) fn lcg_next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    (*state >> 33) as u32
}

pub(crate) fn lcg_f32(state: &mut u64, lo: f32, hi: f32) -> f32 {
    let u = lcg_next(state) as f32 / u32::MAX as f32;
    lo + u * (hi - lo)
}

/// High-frequency deterministic noise input — the WORST case for the stencil
/// tier's manual-bilinear-vs-hardware-filter gap (neighbouring texels differ by
/// up to the full range, so any filter-weight difference is maximally visible).
/// LCG-seeded, reproducible.
pub(crate) fn noise_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    let mut state = 0x5EED_5EEDu64;
    for v in px.iter_mut() {
        *v = f16::from_f32((lcg_next(&mut state) & 0xFFFF) as f32 / 65535.0);
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "freeze-proof-noise",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Render an EFFECT def for `frames` frames through the state-aware executor
/// (feedback loops warm up across frames), source pre-bound to `input`, and
/// return the texture feeding `final_output` after the last frame.
pub(crate) fn render_effect_frames_with_state(
    device: &std::sync::Arc<GpuDevice>,
    registry: &PrimitiveRegistry,
    def: &EffectGraphDef,
    input: &GpuTexture,
    frames: u32,
) -> RenderTarget {
    use crate::node_graph::StateStore;
    let (w, h) = (input.width, input.height);
    let mut graph = def.clone().into_graph(registry, &crate::node_graph::mesh_change::PreparedMeshRules::default()).expect("graph builds");
    let plan = compile(&graph).expect("compiles");
    let src_res = resource_for_output(&plan, find_node(&graph, "system.source"), "out");
    let final_id = find_node(&graph, "system.final_output");
    let out_res = plan
        .steps()
        .iter()
        .find(|s| s.node == final_id)
        .and_then(|s| s.inputs.iter().find(|(n, _)| *n == "in").map(|(_, r)| *r))
        .expect("final_output consumes a texture");

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-fb-src");
    {
        let mut e = device.create_encoder("freeze-fb-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-fb-out");
    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    backend.pre_bind_texture_2d(src_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(out_res, out_rt);
    crate::node_graph::pre_allocate_resources(&mut graph, &plan, device, &mut backend)
        .expect("pre-allocate");

    let mut exec = Executor::new(Box::new(backend));
    let mut state = StateStore::new();
    for i in 0..frames {
        let ft = FrameTime {
            beats: Beats(f64::from(i) / 30.0),
            seconds: Seconds(f64::from(i) / 60.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(i),
        };
        let mut enc = device.create_encoder("freeze-fb-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }

    let result = RenderTarget::new(device, w, h, FMT, "freeze-fb-result");
    let out_tex = exec.backend().texture_2d(out_slot).expect("output retained");
    {
        let mut e = device.create_encoder("freeze-fb-copy");
        e.copy_texture_to_texture(&out_tex.clone(), &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

/// Warm a generator preset's feedback loop for 8 frames and capture the final.
pub(crate) fn render_generator_8_frames(
    def: EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
) -> RenderTarget {
    use crate::preset_context::PresetContext;
    use crate::preset_runtime::PresetRuntime;
    let ctx = |t: f64| PresetContext {
        time: t,
        beat: t * 2.0,
        dt: 1.0 / 60.0,
        width: w,
        height: h,
        output_width: w,
        output_height: h,
        aspect: w as f32 / h as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: 0,
        anim_progress: 0.0,
        trigger_count: 0,
    };
    let mut g = PresetRuntime::from_def_with_device(def, registry, std::sync::Arc::clone(device), w, h, FMT, None)
        .expect("preset builds");
    let target = RenderTarget::new(device, w, h, FMT, "freeze-gen-fusion");
    for i in 0..8u32 {
        let mut enc = device.create_encoder("freeze-gen-fusion");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            g.render(&mut gpu, &target.texture, &ctx(i as f64 / 60.0), &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
    }
    target
}

/// Remove the `reset_trigger` wire feeding `seed_node_id` — a `@reset_gated`
/// kernel with that input unwired runs every frame (the gate is inert). Used to
/// build the "ungated" baseline for the seed-gate equivalence proofs. Addresses
/// the node by stable `node_id` (the def must be flattened first — grouping
/// nests the seed node and prefixes its handle, but `node_id` survives).
pub(crate) fn strip_reset_wire(def: &mut EffectGraphDef, seed_node_id: &str) {
    let Some(id) = def
        .nodes
        .iter()
        .find(|n| n.node_id.as_str() == seed_node_id)
        .map(|n| n.id)
    else {
        return;
    };
    def.wires.retain(|w| !(w.to_node == id && w.to_port == "reset_trigger"));
}

/// Cap every `max_capacity` / `active_count` param in `def` at `cap` — the
/// particle-pool shrink the FluidSim sweep uses, for tests whose subject is
/// texture-domain and doesn't depend on pool size.
pub(crate) fn shrink_particle_pool(def: &mut EffectGraphDef, cap: i32) {
    use manifold_core::effect_graph_def::SerializedParamValue;
    for node in &mut def.nodes {
        for key in ["max_capacity", "active_count"] {
            if node.params.contains_key(key) {
                node.params
                    .insert(key.to_string(), SerializedParamValue::Int { value: cap });
            }
        }
    }
}

/// Drive `def` through the RAW executor (standalone instantiate, identical on
/// both sides of an A/B — generator_input stays at defaults) for `frames`
/// frames, previewing the node `pick` selects so its output survives the last
/// frame. Returns the previewed texture copied out, plus its dims.
pub(crate) fn render_def_capture_node(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
    frames: u32,
    pick: &dyn Fn(&EffectGraphDef) -> u32,
) -> Option<(RenderTarget, (u32, u32))> {
    render_def_capture_node_host(def, registry, device, w, h, frames, pick, false)
}

/// `host_params = true` additionally drives the `system.generator_input`
/// host params (time / beat / aspect / output dims) per frame the way the
/// production `PresetRuntime` path does — the discriminating variable
/// between the raw harness and production.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_def_capture_node_host(
    def: &EffectGraphDef,
    registry: &PrimitiveRegistry,
    device: &std::sync::Arc<GpuDevice>,
    w: u32,
    h: u32,
    frames: u32,
    pick: &dyn Fn(&EffectGraphDef) -> u32,
    host_params: bool,
) -> Option<(RenderTarget, (u32, u32))> {
    use crate::node_graph::{
        BoundaryHandling, HandleScope, instantiate_def, pre_allocate_resources,
    };
    use crate::node_graph::ParamValue;
    use crate::node_graph::{Graph, StateStore};

    let mut graph = Graph::new();
    let inst = instantiate_def(
        &mut graph,
        def,
        registry,
        HandleScope::Global,
        BoundaryHandling::Standalone,
    &crate::node_graph::mesh_change::PreparedMeshRules::default())
    .ok()?;
    let plan = compile(&graph).ok()?;
    let target_inst = *inst.id_map.get(&pick(def))?;
    let gen_in = def
        .nodes
        .iter()
        .find(|n| n.type_id == "system.generator_input")
        .and_then(|n| inst.id_map.get(&n.id).copied());

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), w, h, FMT);
    pre_allocate_resources(&mut graph, &plan, device, &mut backend).ok()?;
    let mut exec = Executor::new(Box::new(backend));
    exec.set_preview_target(Some(target_inst));
    let mut state = StateStore::new();
    for i in 0..frames {
        let t = f64::from(i) / 60.0;
        if host_params && let Some(gi) = gen_in {
            for (name, v) in [
                ("time", t as f32),
                ("beat", (t * 2.0) as f32),
                ("aspect", w as f32 / h as f32),
                ("output_width", w as f32),
                ("output_height", h as f32),
            ] {
                let _ = graph.set_param(gi, name, ParamValue::Float(v));
            }
        }
        let ft = FrameTime {
            seconds: Seconds(t),
            beats: Beats(t * 2.0),
            delta: Seconds(1.0 / 60.0),
            frame_count: i64::from(i),
        };
        let mut enc = device.create_encoder("diag-frame");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, device);
            exec.execute_frame_with_state(&mut graph, &plan, ft, &mut gpu, &mut state, 0);
        }
        enc.commit_and_wait_completed();
    }
    let res = exec.preview_resource()?;
    let slot = exec.backend().slot_for(res)?;
    let tex = exec.backend().texture_2d(slot)?;
    let dims = (tex.width, tex.height);
    let out = RenderTarget::new(device, tex.width, tex.height, tex.format, "diag-capture");
    let mut enc = device.create_encoder("diag-copy");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        gpu.copy_texture_to_texture(tex, &out.texture, tex.width, tex.height);
    }
    enc.commit_and_wait_completed();
    Some((out, dims))
}

/// Render an effect graph whose bound output is at a REDUCED resolution
/// (`out_w` × `out_h`), for multi-resolution fusion proofs. Same shape as
/// [`render_graph`] but the output target — and the copy-out — are sized to the
/// element-space the producer actually writes (e.g. a quarter-res chain below a
/// downsample), so a fused node that failed to inherit that scale would mismatch.
pub(crate) fn render_graph_at(
    device: &std::sync::Arc<GpuDevice>,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
    out_w: u32,
    out_h: u32,
) -> RenderTarget {
    let (sw, sh) = (input.width, input.height);
    let src_rt = RenderTarget::new(device, sw, sh, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, sw, sh, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, out_w, out_h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(std::sync::Arc::clone(device), sw, sh, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, out_w, out_h, FMT, "freeze-graph-result");
    let out_tex = exec.backend().texture_2d(out_slot).expect("graph output retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, out_w, out_h, 1);
        e.commit_and_wait_completed();
    }
    result
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FusedGainU {
    product: f32,
    _pad: [f32; 3],
}
