//! `preview-report` — generate an illustrated guidance report for a preset.
//!
//! Loads a preset (effect or generator) headless, warms its simulation up so
//! feedback graphs have something to show, then snapshots each top-level group's
//! output — applying the same semantic preview encoding the editor shows (a flow
//! field as a colour wheel, a scalar as a black-floor lift, and so on) via the
//! shared [`PreviewEncoder`]. Writes one PNG per group plus a markdown report
//! that pairs each image with the group's plain-language node description.
//!
//! Usage:
//!   preview-report <preset>            # name (e.g. OilyFluid) or path to a .json
//!   preview-report <preset> --frames N # warm-up frames before capture (default 240)
//!   preview-report <preset> --res W    # square capture resolution (default 512)
//!   preview-report <preset> --nodes    # also snapshot every top-level node, not just groups
//!
//! Output lands in `report/<Preset>/` (PNGs + report.md), relative to the cwd.

use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use manifold_core::NodeId;
use manifold_core::effect_graph_def::EffectGraphDef;
use manifold_core::flatten::{flatten_groups, group_output_producer_map};
use manifold_gpu::{GpuDevice, GpuTextureFormat};
use manifold_renderer::gpu_encoder::GpuEncoder;
use manifold_renderer::gpu_readback::ReadbackRequest;
use manifold_renderer::generators::json_graph_generator::JsonGraphGenerator;
use manifold_renderer::node_graph::{PrimitiveRegistry, PreviewEncoder, PreviewEncoding, descriptor_for};
use manifold_renderer::preset_context::{MAX_GEN_PARAMS, PresetContext};
use manifold_renderer::render_target::RenderTarget;
use manifold_renderer::tonemap::{TonemapPipeline, TonemapSettings};

const SIM_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;
const OUT_FORMAT: GpuTextureFormat = GpuTextureFormat::Rgba8Unorm;

struct Args {
    preset: String,
    frames: u32,
    res: u32,
    nodes: bool,
    params: Option<PathBuf>,
}

fn parse_args() -> Args {
    let mut preset = None;
    let mut frames = 240u32;
    let mut res = 512u32;
    let mut nodes = false;
    let mut params = None;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frames" => frames = it.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--res" => res = it.next().and_then(|v| v.parse().ok()).unwrap_or(res),
            "--nodes" => nodes = true,
            "--params" => params = it.next().map(PathBuf::from),
            _ => preset = Some(a),
        }
    }
    let preset = preset.unwrap_or_else(|| {
        eprintln!("usage: preview-report <preset> [--frames N] [--res W] [--nodes] [--params <id:value json>]");
        std::process::exit(2);
    });
    Args { preset, frames, res, nodes, params }
}

/// Resolve the outer-card param values for the render context: each preset param
/// in declared order, taking an override from the `{id: value}` JSON when given,
/// else the param's own default. Returns the array and the count to mark live.
fn build_params(def: &EffectGraphDef, overrides_path: Option<&Path>) -> ([f32; MAX_GEN_PARAMS], u32) {
    let mut arr = [0.0f32; MAX_GEN_PARAMS];
    let overrides: std::collections::HashMap<String, f32> = overrides_path
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let Some(meta) = def.preset_metadata.as_ref() else {
        return (arr, 0);
    };
    for (i, spec) in meta.params.iter().enumerate().take(MAX_GEN_PARAMS) {
        arr[i] = overrides.get(&spec.id).copied().unwrap_or(spec.default_value);
    }
    (arr, meta.params.len().min(MAX_GEN_PARAMS) as u32)
}

/// Resolve a preset arg to a JSON path: a direct path, or a name looked up in
/// the dev asset dirs under this crate's manifest.
fn resolve_preset(arg: &str) -> Option<PathBuf> {
    let direct = Path::new(arg);
    if direct.is_file() {
        return Some(direct.to_path_buf());
    }
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let stem = arg.trim_end_matches(".json");
    for sub in ["assets/effect-presets", "assets/generator-presets"] {
        let p = manifest.join(sub).join(format!("{stem}.json"));
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn slugify(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect::<String>()
        .trim_matches('_')
        .to_string()
}

fn ctx_at(frame: u32, res: u32, params: &[f32; MAX_GEN_PARAMS], param_count: u32) -> PresetContext {
    let dt = 1.0 / 60.0;
    PresetContext {
        time: frame as f64 * dt,
        beat: frame as f64 * dt * 2.0, // 120 bpm, arbitrary but moving
        dt: dt as f32,
        width: res,
        height: res,
        output_width: res,
        output_height: res,
        aspect: 1.0,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: 0.0,
        trigger_count: 0,
        params: *params,
        param_count,
    }
}

/// One image to capture: a label, the node id to preview (a group id resolves to
/// its producer inside the generator), and the producing node's type id for the
/// description lookup.
struct Capture {
    title: String,
    node_id: Option<NodeId>, // None = the final output (whole effect)
    type_id: Option<String>,
    /// The group's own one-line purpose, if the author wrote one. Preferred
    /// over the producer atom's tooltip, which describes the wrong altitude.
    description: Option<String>,
}

fn main() {
    let args = parse_args();
    let path = resolve_preset(&args.preset).unwrap_or_else(|| {
        eprintln!("preset not found: {}", args.preset);
        std::process::exit(2);
    });

    let bytes = std::fs::read_to_string(&path).expect("read preset");
    let def: EffectGraphDef = serde_json::from_str(&bytes).expect("parse preset");
    let preset_name = def.name.clone().unwrap_or_else(|| {
        path.file_stem().unwrap().to_string_lossy().to_string()
    });

    let res = args.res;
    let registry = PrimitiveRegistry::with_builtin();
    let device = GpuDevice::new();

    let mut generator = JsonGraphGenerator::from_def_with_device(
        def.clone(), &registry, &device, res, res, SIM_FORMAT,
    )
    .expect("build generator");

    let (param_arr, param_count) = build_params(&def, args.params.as_deref());
    if param_count > 0 {
        println!("applying {param_count} outer-card params{}",
            if args.params.is_some() { " (with overrides)" } else { " (defaults)" });
    }

    let encoder = PreviewEncoder::new(&device, OUT_FORMAT);
    let sim_out = RenderTarget::new(&device, res, res, SIM_FORMAT, "report-sim-out");
    let color = RenderTarget::new(&device, res, res, OUT_FORMAT, "report-color");
    // The hero output is tonemapped through the same ACES path the live display
    // uses, so the report's "Output" image matches what is on screen rather than
    // the raw HDR buffer (which reads dark for low-amplitude sims).
    let tonemap = TonemapPipeline::new(&device, res, res);

    // Build the type-id lookup over the flattened graph so a group's producer
    // (or any node) can be described from its descriptor summary.
    let flat = flatten_groups(&def).unwrap_or_else(|_| def.clone());
    let type_of: std::collections::HashMap<NodeId, String> = flat
        .nodes
        .iter()
        .map(|n| (n.node_id.clone(), n.type_id.clone()))
        .collect();
    let producer_of: std::collections::HashMap<NodeId, NodeId> = group_output_producer_map(&def)
        .into_iter()
        .map(|(group, producer, _port)| (group, producer))
        .collect();

    // The capture list, in document order: the whole output first ("Output", a
    // clean token for the guidance doc), then each top-level group (or every
    // top-level node with --nodes).
    // Read the authored guidance up front so any `preview://node:<id>` tokens
    // (an inner node previewed directly, e.g. a seed pattern before its gain)
    // get captured alongside the groups.
    let guidance = guidance_sidecar(&path);
    let node_targets = guidance.as_deref().map(node_tokens).unwrap_or_default();

    let mut captures = vec![Capture {
        title: "Output".to_string(),
        node_id: None,
        type_id: None,
        description: None,
    }];
    for node in &def.nodes {
        let is_group = node.type_id == "group";
        let is_node = !node.type_id.starts_with("system.") && !is_group;
        if is_group || (args.nodes && is_node) {
            let title = node.handle.clone().unwrap_or_else(|| node.node_id.to_string());
            // A group describes via its producer; a plain node, via itself.
            let producer = producer_of.get(&node.node_id).cloned().unwrap_or_else(|| node.node_id.clone());
            captures.push(Capture {
                title,
                node_id: Some(node.node_id.clone()),
                type_id: type_of.get(&producer).cloned(),
                description: node.group.as_ref().and_then(|g| g.description.clone()),
            });
        }
    }
    // Inner-node targets requested by the guidance doc, addressed by stable nodeId.
    for id in &node_targets {
        let nid = NodeId::new(id);
        captures.push(Capture {
            title: format!("node:{id}"),
            node_id: Some(nid.clone()),
            type_id: type_of.get(&nid).cloned(),
            description: None,
        });
    }

    // Warm the simulation up (no preview target) so feedback graphs develop.
    generator.set_preview_node(None);
    for f in 0..args.frames {
        render_frame(&mut generator, &device, &sim_out, &ctx_at(f, res, &param_arr, param_count));
    }
    println!("warmed up {} frames at {res}x{res}", args.frames);

    let out_dir = PathBuf::from("report").join(slugify(&preset_name));
    std::fs::create_dir_all(&out_dir).expect("create out dir");
    // Clear stale PNGs so a rename or regroup doesn't leave orphans behind.
    if let Ok(entries) = std::fs::read_dir(&out_dir) {
        for e in entries.flatten() {
            if e.path().extension().and_then(|x| x.to_str()) == Some("png") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }

    // Capture each image; collect what the report layout needs.
    let mut shots: Vec<Shot> = Vec::new();
    let mut frame = args.frames;
    for (i, cap) in captures.iter().enumerate() {
        frame += 1;
        generator.set_preview_node(cap.node_id.as_ref());
        render_frame(&mut generator, &device, &sim_out, &ctx_at(frame, res, &param_arr, param_count));

        let is_hero = cap.node_id.is_none();
        let (source_tex, encoding): (Option<&manifold_gpu::GpuTexture>, PreviewEncoding) = if is_hero
        {
            // Tonemap the HDR output on its own command buffer — the on-screen image.
            let mut native = device.create_encoder("report-tonemap");
            {
                let mut gpu = GpuEncoder::new(&mut native, &device);
                tonemap.apply(&mut gpu, &sim_out.texture, &TonemapSettings::default());
            }
            native.commit_and_wait_completed();
            (Some(&tonemap.output.texture), PreviewEncoding::Color)
        } else {
            (generator.preview_texture(), generator.preview_encoding())
        };

        let Some(source_tex) = source_tex else {
            println!("  {} — no image output, skipped", cap.title);
            continue;
        };

        // A raw-colour intermediate (not the hero) reads near-black for a
        // low-amplitude sim, so lift it with the same asinh curve the flow
        // encodings use. The hero stays tonemapped to match the screen.
        let render_encoding = if !is_hero && encoding == PreviewEncoding::Color {
            PreviewEncoding::ScalarLift
        } else {
            encoding
        };

        let pixels = encode_and_read(&device, &encoder, source_tex, &color, render_encoding, res);
        let fname = format!("{:02}_{}.png", i, slugify(&cap.title));
        write_png(&out_dir.join(&fname), res, res, &pixels);

        let blurb = cap.description.clone().or_else(|| {
            cap.type_id.as_deref().and_then(descriptor_for).map(|d| d.summary.to_string())
        });
        shots.push(Shot { title: cap.title.clone(), fname, encoding: render_encoding, blurb });
        println!("  captured {} ({})", cap.title, encoding_label(render_encoding));
    }

    // Build the report. If an authored guidance doc sits next to the preset
    // (`<stem>.guidance.md`), inject the captured images into it at
    // `](preview://Title)` tokens; otherwise emit the auto-generated report.
    let report = match &guidance {
        Some(doc) => {
            println!("using authored guidance: {}", sidecar_path(&path).display());
            inject_into_guidance(doc, &shots)
        }
        None => auto_report(&preset_name, &path, &def, &shots),
    };
    let report_path = out_dir.join("report.md");
    std::fs::write(&report_path, report).expect("write report");
    println!("\nwrote {}", report_path.display());
}

/// One captured group image and the metadata the report needs to place it.
struct Shot {
    title: String,
    fname: String,
    encoding: PreviewEncoding,
    blurb: Option<String>,
}

/// `<dir>/<stem>.guidance.md` next to the preset JSON.
fn sidecar_path(preset: &Path) -> PathBuf {
    let stem = preset.file_stem().unwrap_or_default().to_string_lossy();
    preset.with_file_name(format!("{stem}.guidance.md"))
}

fn guidance_sidecar(preset: &Path) -> Option<String> {
    std::fs::read_to_string(sidecar_path(preset)).ok()
}

/// Collect the inner-node ids referenced by `](preview://node:<id>)` tokens in
/// an authored guidance doc, so they can be captured directly (a group shows its
/// output; this shows one chosen node inside it).
fn node_tokens(doc: &str) -> Vec<String> {
    const NEEDLE: &str = "preview://node:";
    let mut ids: Vec<String> = Vec::new();
    let mut rest = doc;
    while let Some(i) = rest.find(NEEDLE) {
        let after = &rest[i + NEEDLE.len()..];
        let id: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        rest = &after[id.len()..];
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Replace every `](preview://Title)` token in the authored doc with the real
/// image path. Tokens with no matching capture are left untouched (visible as a
/// broken link, so the gap is obvious) and reported.
fn inject_into_guidance(doc: &str, shots: &[Shot]) -> String {
    let mut out = doc.to_string();
    for s in shots {
        // Swap the token for the image path and append a short caption naming the
        // smart-preview encoding. The key (above) defines what each one means.
        let repl = format!("]({})\n\n*{}.*", s.fname, encoding_name(s.encoding));
        out = out.replace(&format!("](preview://{})", s.title), &repl);
    }
    out = out.replace("[[preview-legend]]", &build_legend(shots));
    if let Some(idx) = out.find("](preview://") {
        let tail = &out[idx..(idx + 60).min(out.len())];
        eprintln!("warning: unmatched guidance image token near `{tail}`");
    }
    out
}

/// A legend covering the smart-preview encodings actually used in this report,
/// substituted for a `[[preview-legend]]` token in the guidance doc.
fn build_legend(shots: &[Shot]) -> String {
    const ORDER: [PreviewEncoding; 6] = [
        PreviewEncoding::Color,
        PreviewEncoding::ScalarLift,
        PreviewEncoding::ScalarSigned,
        PreviewEncoding::VectorField,
        PreviewEncoding::Normal,
        PreviewEncoding::Depth,
    ];
    let mut out = String::from(
        "Each image uses the graph editor's smart preview, which maps the underlying data \
         to color so it can be read at a glance. The encodings that appear in this report:\n",
    );
    for e in ORDER {
        if shots.iter().any(|s| s.encoding == e) {
            out.push_str(&format!("\n- **{}**: {}", encoding_name(e), encoding_legend(e)));
        }
    }
    out.push_str("\n\nEach image below is labelled with the encoding it uses.\n");
    out
}

/// Fallback report for presets with no authored guidance doc: one section per
/// captured group, image plus its description.
fn auto_report(preset_name: &str, path: &Path, def: &EffectGraphDef, shots: &[Shot]) -> String {
    let mut report = String::new();
    report.push_str(&format!("# {preset_name} — node guidance report\n\n"));
    report.push_str(&format!(
        "Auto-generated from `{}`. Each image is the live output of that group, \
         shown with the smart-preview encoding the graph editor uses.\n\n",
        path.file_name().unwrap().to_string_lossy()
    ));
    if let Some(desc) = &def.description {
        report.push_str(&format!("> {}\n\n", first_sentence(desc)));
    }
    for s in shots {
        report.push_str(&format!("## {}\n\n", s.title));
        report.push_str(&format!("![{}]({})\n\n", s.title, s.fname));
        report.push_str(&format!("*Encoding: {}*\n\n", encoding_label(s.encoding)));
        if let Some(blurb) = &s.blurb {
            report.push_str(&format!("{blurb}\n\n"));
        }
    }
    report
}

/// Render one generator frame onto `target`, committing and waiting so the
/// preview texture is populated and readable afterwards.
fn render_frame(
    generator: &mut JsonGraphGenerator,
    device: &GpuDevice,
    target: &RenderTarget,
    ctx: &PresetContext,
) {
    let mut native = device.create_encoder("report-render");
    {
        let mut gpu = GpuEncoder::new(&mut native, device);
        generator.render(&mut gpu, &target.texture, ctx);
    }
    native.commit_and_wait_completed();
}

/// Encode `source` into the 8-bit colour target via the shared encoder, read it
/// back to CPU, and return tightly-packed RGBA8 pixels (`res*res*4`).
fn encode_and_read(
    device: &GpuDevice,
    encoder: &PreviewEncoder,
    source: &manifold_gpu::GpuTexture,
    color: &RenderTarget,
    encoding: PreviewEncoding,
    res: u32,
) -> Vec<u8> {
    let mut readback = ReadbackRequest::new();
    let mut native = device.create_encoder("report-encode");
    encoder.encode(&mut native, source, &color.texture, encoding, true);
    {
        let mut gpu = GpuEncoder::new(&mut native, device);
        readback.submit(&mut gpu, &color.texture, res, res);
    }
    native.commit_and_wait_completed();
    readback.try_read().expect("readback")
}

fn write_png(path: &Path, w: u32, h: u32, rgba8: &[u8]) {
    let file = File::create(path).expect("create png");
    let mut enc = png::Encoder::new(BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba8).expect("png data");
}

fn encoding_label(e: PreviewEncoding) -> &'static str {
    match e {
        PreviewEncoding::Color => "colour (raw)",
        PreviewEncoding::ScalarLift => "scalar lift",
        PreviewEncoding::ScalarSigned => "signed diverging",
        PreviewEncoding::VectorField => "vector field (flow wheel)",
        PreviewEncoding::Normal => "normal map",
        PreviewEncoding::Depth => "depth ramp",
    }
}

/// Short name of an encoding for the legend heading and per-image caption.
fn encoding_name(e: PreviewEncoding) -> &'static str {
    match e {
        PreviewEncoding::Color => "Raw color",
        PreviewEncoding::ScalarLift => "Brightness lift",
        PreviewEncoding::ScalarSigned => "Signed scale",
        PreviewEncoding::VectorField => "Flow wheel",
        PreviewEncoding::Normal => "Normal map",
        PreviewEncoding::Depth => "Depth ramp",
    }
}

/// Full legend description of what an encoding's colors mean.
fn encoding_legend(e: PreviewEncoding) -> &'static str {
    match e {
        PreviewEncoding::Color => {
            "the image shown as displayed. The final output is tonemapped to match the live screen."
        }
        PreviewEncoding::ScalarLift => {
            "the real channel values with dark areas raised by a fixed curve, so faint detail shows instead of reading as solid black."
        }
        PreviewEncoding::VectorField => {
            "a flow or force drawn as color. The hue at each pixel gives the direction it points and the brightness gives the speed, so black is no motion."
        }
        PreviewEncoding::ScalarSigned => {
            "values that cross zero. Blue is negative, black is zero, red is positive."
        }
        PreviewEncoding::Normal => {
            "surface directions encoded as a standard blue-dominant normal map."
        }
        PreviewEncoding::Depth => "distance from near to far on a perceptual color ramp.",
    }
}

fn first_sentence(s: &str) -> String {
    let s = s.trim();
    match s.find(". ") {
        Some(i) => s[..=i].trim().to_string(),
        None => s.lines().next().unwrap_or(s).to_string(),
    }
}
