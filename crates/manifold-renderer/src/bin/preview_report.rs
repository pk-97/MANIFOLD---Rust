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
}

fn parse_args() -> Args {
    let mut preset = None;
    let mut frames = 240u32;
    let mut res = 512u32;
    let mut nodes = false;
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--frames" => frames = it.next().and_then(|v| v.parse().ok()).unwrap_or(frames),
            "--res" => res = it.next().and_then(|v| v.parse().ok()).unwrap_or(res),
            "--nodes" => nodes = true,
            _ => preset = Some(a),
        }
    }
    let preset = preset.unwrap_or_else(|| {
        eprintln!("usage: preview-report <preset> [--frames N] [--res W] [--nodes]");
        std::process::exit(2);
    });
    Args { preset, frames, res, nodes }
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

fn ctx_at(frame: u32, res: u32) -> PresetContext {
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
        params: [0.0; MAX_GEN_PARAMS],
        param_count: 0,
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

    // The capture list, in document order: the whole output first, then each
    // top-level group (or every top-level node with --nodes).
    let mut captures = vec![Capture {
        title: format!("{preset_name} — Output"),
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

    // Warm the simulation up (no preview target) so feedback graphs develop.
    generator.set_preview_node(None);
    for f in 0..args.frames {
        render_frame(&mut generator, &device, &sim_out, &ctx_at(f, res));
    }
    println!("warmed up {} frames at {res}x{res}", args.frames);

    let out_dir = PathBuf::from("report").join(slugify(&preset_name));
    std::fs::create_dir_all(&out_dir).expect("create out dir");

    let mut report = String::new();
    report.push_str(&format!("# {preset_name} — node guidance report\n\n"));
    report.push_str(&format!(
        "Auto-generated from `{}`. Each image is the live output of that group, \
         shown with the same smart-preview encoding the graph editor uses.\n\n",
        path.file_name().unwrap().to_string_lossy()
    ));
    if let Some(desc) = &def.description {
        report.push_str(&format!("> {}\n\n", first_sentence(desc)));
    }

    let mut frame = args.frames;
    for (i, cap) in captures.iter().enumerate() {
        frame += 1;
        generator.set_preview_node(cap.node_id.as_ref());
        render_frame(&mut generator, &device, &sim_out, &ctx_at(frame, res));

        // For the whole-output capture there is no preview node: encode the
        // generator's own final output texture (raw colour). Otherwise encode
        // the captured node texture with its semantic encoding.
        let (source_tex, encoding): (Option<&manifold_gpu::GpuTexture>, PreviewEncoding) =
            if cap.node_id.is_none() {
                // Tonemap the generator's HDR output on its own command buffer,
                // then encode the SDR result raw — this is the on-screen image.
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

        let pixels = encode_and_read(&device, &encoder, source_tex, &color, encoding, res);
        let fname = format!("{:02}_{}.png", i, slugify(&cap.title));
        write_png(&out_dir.join(&fname), res, res, &pixels);

        report.push_str(&format!("## {}\n\n", cap.title));
        report.push_str(&format!("![{}]({})\n\n", cap.title, fname));
        report.push_str(&format!("*Encoding: {}*\n\n", encoding_label(encoding)));
        // Prefer the group's own authored purpose; fall back to the producer
        // atom's tooltip (right node, wrong altitude, but better than nothing).
        let blurb = cap.description.clone().or_else(|| {
            cap.type_id.as_deref().and_then(descriptor_for).map(|d| d.summary.to_string())
        });
        if let Some(blurb) = blurb {
            report.push_str(&format!("{blurb}\n\n"));
        }
        println!("  captured {} ({})", cap.title, encoding_label(encoding));
    }

    let report_path = out_dir.join("report.md");
    std::fs::write(&report_path, report).expect("write report");
    println!("\nwrote {}", report_path.display());
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

fn first_sentence(s: &str) -> String {
    let s = s.trim();
    match s.find(". ") {
        Some(i) => s[..=i].trim().to_string(),
        None => s.lines().next().unwrap_or(s).to_string(),
    }
}
