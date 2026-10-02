//! `manifold frame-time <project.manifold> --frames N [--resolution R]
//! [--stamp-every K] [--splash-frames S] [--png-frame T --png <path>]` —
//! the whole frame of a real project the way the app runs it: the
//! production loader, the headless content thread, the project's own frame
//! rate and output size, from the clip's start. Every frame reports what
//! Peter's FPS counter sees (the wall interval between ticks) and the GPU
//! surface wait; every `K`th frame also carries per-dispatch GPU timestamps
//! split per node type, per dispatch label inside the solver and the
//! whitewater, and per pass label inside render_scene. Timestamped frames
//! open one encoder per dispatch with encode replay off, so their split is a
//! ratio, never the budget. `--stamp-granularity node` keeps one sampled
//! encoder per graph step instead, so the per-node-type table is the plain
//! frame's breakdown (replay still off; the inner tables are then empty).
//! A timestamped frame waits for its GPU work, a plain frame does not, and
//! a liquid coupled to a body runs no tick while the last tick's reaction
//! is in flight: with plain and timestamped frames interleaved on such a
//! project the timestamped frames skip the step once the plain frame's GPU
//! work outlasts the tick interval (every one of them at 128). Measure a
//! coupled project with `--stamp-every 1`.
//! The first `S` frames are the splash, the rest
//! the calm; both tables print. `--resolution` overrides the GPU FLIP
//! generator's `resolution` card in memory, never on disk.
//! Presentation is not timed here (no window); the pacing doc's vsync
//! quantisation applies on top of these numbers.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use manifold_gpu::ProfileGranularity;

use crate::content_command::ContentCommand;
use crate::perf_soak::{prepare_project_edited, PreparedProject};

const GENERATOR: &str = "WaterDamBreakGpuFlip";
const RESOLUTION_PARAM: &str = "resolution";
const STEP: &str = "node.gpu_flip_step";
const WHITEWATER: &str = "node.whitewater_step";
const RENDER: &str = "node.render_scene";
/// Spans a timestamped frame may hold; a 128 solve dispatches thousands.
const MAX_SPANS: usize = 32768;

struct Args {
    project: String,
    frames: usize,
    resolution: Option<f32>,
    stamp_every: usize,
    granularity: ProfileGranularity,
    splash_frames: usize,
    png: Option<(usize, String)>,
}

fn usage_exit(msg: &str) -> ! {
    eprintln!("frame-time: {msg}");
    eprintln!(
        "usage: manifold frame-time <project.manifold> --frames N [--resolution R] \
         [--stamp-every K] [--stamp-granularity dispatch|node] [--splash-frames S] \
         [--png-frame T --png <path>]"
    );
    std::process::exit(2);
}

/// `--stamp-granularity` values: `dispatch` (default) or `node`.
fn granularity(s: Option<&str>) -> Result<ProfileGranularity, String> {
    match s {
        None | Some("dispatch") => Ok(ProfileGranularity::Dispatch),
        Some("node") => Ok(ProfileGranularity::Tag),
        Some(other) => Err(format!("--stamp-granularity must be dispatch or node, not {other}")),
    }
}

fn value(args: &[String], flag: &str) -> Option<String> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned()
}

fn parse(args: &[String]) -> Args {
    let project = match args.get(1) {
        Some(p) if !p.starts_with("--") => p.clone(),
        _ => usage_exit("missing <project.manifold> argument"),
    };
    let number = |flag: &str, default: Option<usize>| -> usize {
        match value(args, flag) {
            Some(s) => s.parse().unwrap_or_else(|_| usage_exit(&format!("{flag} must be a whole number"))),
            None => default.unwrap_or_else(|| usage_exit(&format!("{flag} is required"))),
        }
    };
    let frames = number("--frames", None);
    let stamp_every = number("--stamp-every", Some(5)).max(1);
    let granularity = granularity(value(args, "--stamp-granularity").as_deref()).unwrap_or_else(|e| usage_exit(&e));
    let splash_frames = number("--splash-frames", Some(frames / 2));
    let resolution = value(args, "--resolution")
        .map(|s| s.parse::<f32>().unwrap_or_else(|_| usage_exit("--resolution must be a number")));
    let png = match (value(args, "--png-frame"), value(args, "--png")) {
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => usage_exit("--png-frame and --png go together"),
        (Some(_), Some(path)) => Some((number("--png-frame", None), path)),
    };
    Args { project, frames, resolution, stamp_every, granularity, splash_frames, png }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_granularity_flag_parses() {
        assert_eq!(granularity(None), Ok(ProfileGranularity::Dispatch));
        assert_eq!(granularity(Some("dispatch")), Ok(ProfileGranularity::Dispatch));
        assert_eq!(granularity(Some("node")), Ok(ProfileGranularity::Tag));
        assert!(granularity(Some("step")).is_err());
    }
}

/// One frame's numbers. Every frame has the wall interval and the surface
/// wait; a timestamped frame also has its split.
struct Frame {
    interval_ms: f64,
    fence_ms: f64,
    /// Plain (unprofiled) frames only: true GPU ms of the Generators and
    /// Compositor command buffers, from their completion handlers.
    plain_gpu_ms: Option<(f64, f64)>,
    split: Option<Split>,
}

/// The GPU-clock span covered by one encoder's chunks within a frame.
#[derive(Default)]
struct Span {
    first_start: Option<f64>,
    last_end: f64,
}

impl Span {
    fn cover(&mut self, start: f64, end: f64) {
        self.first_start = Some(self.first_start.map_or(start, |s| s.min(start)));
        self.last_end = self.last_end.max(end);
    }

    fn seen(&self) -> bool {
        self.first_start.is_some()
    }

    fn ms(&self) -> f64 {
        self.first_start.map_or(0.0, |start| (self.last_end - start).max(0.0) * 1e3)
    }
}

#[derive(Clone, Default)]
struct Split {
    /// Whole command buffers (Generators + Compositor), GPU ms.
    total_ms: f64,
    overflow: usize,
    per_type: BTreeMap<String, f64>,
    per_step_label: BTreeMap<String, f64>,
    per_whitewater_label: BTreeMap<String, f64>,
    per_render_label: BTreeMap<String, f64>,
}

fn percentile(samples: &[f64], fraction: f64) -> f64 {
    if samples.is_empty() {
        return f64::NAN;
    }
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted[((sorted.len() - 1) as f64 * fraction).round() as usize]
}

fn print_split(title: &str, columns: &BTreeMap<String, Vec<f64>>) {
    let mut rows: Vec<(&String, &Vec<f64>)> = columns.iter().collect();
    rows.sort_by(|a, b| percentile(b.1, 0.5).total_cmp(&percentile(a.1, 0.5)));
    println!("  {title} (timestamped frames, ratios only):");
    for (name, samples) in rows {
        println!(
            "    {name:<56} p50 {:>8.3} ms  p95 {:>8.3} ms",
            percentile(samples, 0.5),
            percentile(samples, 0.95)
        );
    }
}

#[derive(Default)]
struct Phase {
    interval: Vec<f64>,
    fence: Vec<f64>,
    plain_generators: Vec<f64>,
    plain_compositor: Vec<f64>,
    plain_total: Vec<f64>,
    stamped_total: Vec<f64>,
    overflow: usize,
    per_type: BTreeMap<String, Vec<f64>>,
    per_step: BTreeMap<String, Vec<f64>>,
    per_whitewater: BTreeMap<String, Vec<f64>>,
    per_render: BTreeMap<String, Vec<f64>>,
}

impl Phase {
    fn add(&mut self, frame: &Frame) {
        self.interval.push(frame.interval_ms);
        self.fence.push(frame.fence_ms);
        if let Some((generators, compositor)) = frame.plain_gpu_ms {
            self.plain_generators.push(generators);
            self.plain_compositor.push(compositor);
            self.plain_total.push(generators + compositor);
        }
        let Some(split) = &frame.split else { return };
        self.stamped_total.push(split.total_ms);
        self.overflow += split.overflow;
        for (into, from) in [
            (&mut self.per_type, &split.per_type),
            (&mut self.per_step, &split.per_step_label),
            (&mut self.per_whitewater, &split.per_whitewater_label),
            (&mut self.per_render, &split.per_render_label),
        ] {
            for (name, ms) in from {
                into.entry(name.clone()).or_default().push(*ms);
            }
        }
    }

    fn report(&self, name: &str) {
        println!("== {name} ==");
        println!(
            "  frames ({}): tick interval p50 {:.2} ms p95 {:.2} ms (fps p50 {:.1}) | GPU surface wait p50 {:.2} ms p95 {:.2} ms",
            self.interval.len(),
            percentile(&self.interval, 0.5),
            percentile(&self.interval, 0.95),
            1000.0 / percentile(&self.interval, 0.5),
            percentile(&self.fence, 0.5),
            percentile(&self.fence, 0.95),
        );
        println!(
            "  plain frames ({}): true GPU ms  total p50 {:.2} p95 {:.2} max {:.2} | generators p50 {:.2} p95 {:.2} | compositor p50 {:.2} p95 {:.2}",
            self.plain_total.len(),
            percentile(&self.plain_total, 0.5),
            percentile(&self.plain_total, 0.95),
            percentile(&self.plain_total, 1.0),
            percentile(&self.plain_generators, 0.5),
            percentile(&self.plain_generators, 0.95),
            percentile(&self.plain_compositor, 0.5),
            percentile(&self.plain_compositor, 0.95),
        );
        println!(
            "  timestamped frames ({}): whole-buffer GPU p50 {:.2} ms p95 {:.2} ms, sampler overflow {}",
            self.stamped_total.len(),
            percentile(&self.stamped_total, 0.5),
            percentile(&self.stamped_total, 0.95),
            self.overflow,
        );
        print_split("per node type", &self.per_type);
        print_split("gpu_flip_step per dispatch label", &self.per_step);
        print_split("whitewater_step per dispatch label", &self.per_whitewater);
        print_split("render_scene per pass label", &self.per_render);
    }
}

fn set_profiling(ct: &mut crate::content_thread::ContentThread, on: bool) {
    ct.content_pipeline.set_profiling(on, MAX_SPANS);
    for renderer in ct.engine.renderers_mut() {
        if let Some(generator) = renderer
            .as_any_mut()
            .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
        {
            generator.set_profiling(on);
        }
    }
}

/// Join the frame's GPU spans back to their nodes, the way the frame probe
/// in `gpu_flip_frame_perf.rs` does.
fn split(ct: &mut crate::content_thread::ContentThread) -> Split {
    let gpu_profiles = ct.content_pipeline.take_gpu_profiles();
    let mut steps = ct.content_pipeline.take_step_profiles();
    for renderer in ct.engine.renderers_mut() {
        if let Some(generator) = renderer
            .as_any_mut()
            .downcast_mut::<manifold_renderer::generator_renderer::GeneratorRenderer>()
        {
            steps.extend(generator.take_step_profiles());
        }
    }
    let types: BTreeMap<String, String> = steps.into_iter().map(|s| (s.tag, s.type_id)).collect();
    let mut split = Split::default();
    for (buffer, profile) in &gpu_profiles {
        split.total_ms += profile.total_ms;
        split.overflow += profile.overflow;
        for span in &profile.spans {
            let Some(type_id) = types.get(&span.tag) else {
                *split.per_type.entry(format!("(untagged, {buffer})")).or_insert(0.0) += span.millis;
                continue;
            };
            *split.per_type.entry(type_id.clone()).or_insert(0.0) += span.millis;
            let labelled = match type_id.as_str() {
                STEP => Some(&mut split.per_step_label),
                WHITEWATER => Some(&mut split.per_whitewater_label),
                RENDER => Some(&mut split.per_render_label),
                _ => None,
            };
            if let Some(labelled) = labelled {
                let key = if type_id == RENDER { format!("{:?} {}", span.kind, span.label) } else { span.label.clone() };
                *labelled.entry(key).or_insert(0.0) += span.millis;
            }
        }
    }
    split
}

fn write_png(ct: &mut crate::content_thread::ContentThread, path: &str) {
    let Some(device) = ct.content_pipeline.native_device_handle() else {
        eprintln!("frame-time: no native device for the PNG");
        return;
    };
    let texture = ct.content_pipeline.export_output_texture();
    // The graph tone-maps in-graph (node.tone_map), so display-encode only.
    let png = manifold_renderer::headless_readback::readback_to_srgb_png_linear(&device, texture, texture.width, texture.height);
    match std::fs::write(path, png) {
        Ok(()) => eprintln!("frame-time: wrote {path}"),
        Err(e) => eprintln!("frame-time: write {path}: {e}"),
    }
}

pub fn run(args: &[String]) -> ! {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).try_init();
    let args = parse(args);
    match probe(&args) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            eprintln!("frame-time: {e}");
            std::process::exit(3);
        }
    }
}

fn probe(args: &Args) -> Result<(), String> {
    let mut overridden = 0usize;
    let PreparedProject { mut ct, cmd_tx, cmd_rx, state_tx, drain, width, height, frame_rate, .. } =
        prepare_project_edited(&args.project, "frame-time", &mut |project| {
            let Some(resolution) = args.resolution else { return };
            for layer in &mut project.timeline.layers {
                if !layer.hosts_generator() || layer.generator_type().as_str() != GENERATOR {
                    continue;
                }
                if let Some(params) = layer.gen_params_mut()
                    && params.set_base_param(RESOLUTION_PARAM, resolution)
                {
                    overridden += 1;
                }
            }
        })?;
    if args.resolution.is_some() && overridden == 0 {
        return Err(format!("no {GENERATOR} layer took the resolution override"));
    }
    let resolution_note = args.resolution.map_or("the file's resolution".to_owned(), |r| format!("resolution {r}"));
    let device_name = ct.content_pipeline.native_device().map_or("unknown".to_owned(), |d| d.device_name());
    println!(
        "frame-time: {} at {width}x{height} @ {frame_rate} fps, {resolution_note}, {} frames, every {}th timestamped per {}, splash = first {} frames, on {device_name}",
        args.project,
        args.frames,
        args.stamp_every,
        match args.granularity {
            ProfileGranularity::Dispatch => "dispatch",
            ProfileGranularity::Tag => "node",
        },
        args.splash_frames
    );
    ct.content_pipeline.set_profiling_granularity(args.granularity);

    ct.timer.resume_after_load();
    ct.handle_command(ContentCommand::Play);
    let (gpu_time_tx, gpu_time_rx) = crossbeam_channel::unbounded::<(&'static str, f64, f64)>();
    ct.content_pipeline.set_gpu_time_tap(Some(gpu_time_tx));
    let mut frames: Vec<Frame> = Vec::with_capacity(args.frames);
    let mut last = Instant::now();
    for index in 0..args.frames {
        let stamped = index % args.stamp_every == args.stamp_every - 1;
        set_profiling(&mut ct, stamped);
        if ct.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx) {
            return Err("shutdown requested mid-run".into());
        }
        let now = Instant::now();
        let interval_ms = now.duration_since(last).as_secs_f64() * 1e3;
        last = now;
        let fence_ms = ct.content_pipeline.last_fence_wait_ms();
        let frame_split = stamped.then(|| split(&mut ct));
        if let Some((at, path)) = &args.png
            && *at == index
        {
            write_png(&mut ct, path);
        }
        frames.push(Frame { interval_ms, fence_ms, plain_gpu_ms: None, split: frame_split });
    }
    set_profiling(&mut ct, false);
    ct.content_pipeline.set_gpu_time_tap(None);
    drop(state_tx);
    drain.join().map_err(|_| "drain thread panicked".to_string())?;

    // Completion handlers fire in submission order; the plain frames took
    // them in that same order. A frame is every Generators chunk, then every
    // Compositor chunk (chunking splits each encoder into several buffers);
    // a Generators chunk after a Compositor one starts the next frame. The
    // chunks of one frame overlap on the GPU, so a frame's time is the span
    // from its first start to its last end, not the sum of durations. Let
    // the last buffers land before grouping.
    std::thread::sleep(Duration::from_millis(500));
    let mut plain = frames.iter_mut().filter(|f| f.split.is_none());
    let (mut generators, mut compositor) = (Span::default(), Span::default());
    let mut chunks: Vec<(&'static str, f64, f64)> = Vec::new();
    while let Ok(chunk) = gpu_time_rx.try_recv() {
        chunks.push(chunk);
    }
    for (label, start, end) in chunks {
        let is_generators = label == "Generators";
        if is_generators && compositor.seen() {
            let Some(frame) = plain.next() else { break };
            frame.plain_gpu_ms = Some((generators.ms(), compositor.ms()));
            (generators, compositor) = (Span::default(), Span::default());
        }
        if is_generators { &mut generators } else { &mut compositor }.cover(start, end);
    }
    if compositor.seen() && let Some(frame) = plain.next() {
        frame.plain_gpu_ms = Some((generators.ms(), compositor.ms()));
    }
    let unpaired = frames.iter().filter(|f| f.split.is_none() && f.plain_gpu_ms.is_none()).count();
    if unpaired > 0 {
        println!("  WARNING: {unpaired} plain frames got no GPU completion time");
    }

    println!("  per frame: wall interval ms / true GPU ms (* = timestamped: one encoder per dispatch, no plain GPU time):");
    for (row_index, row) in frames.chunks(8).enumerate() {
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let index = row_index * 8 + i;
                match f.plain_gpu_ms {
                    Some((g, c)) => format!("{index:>3}:{:>6.1}/{:<5.1}", f.interval_ms, g + c),
                    None => format!("{index:>3}:{:>6.1}*     ", f.interval_ms),
                }
            })
            .collect();
        println!("    {}", cells.join(" "));
    }
    let (mut whole, mut splash, mut calm) = (Phase::default(), Phase::default(), Phase::default());
    for (index, frame) in frames.iter().enumerate() {
        whole.add(frame);
        if index < args.splash_frames { splash.add(frame) } else { calm.add(frame) }
    }
    whole.report("whole run");
    splash.report(&format!("splash (frames 0..{})", args.splash_frames));
    calm.report(&format!("calm (frames {}..{})", args.splash_frames, args.frames));
    Ok(())
}
