//! `manifold frame-time <project.manifold> --frames N [--frame-clock] [--resolution R]
//! [--solve-level L] [--sim-rate HZ] [--stamp-every K] [--splash-frames S] [--png-frame T --png <path>]` —
//! the whole frame of a real project the way the app runs it: the
//! production loader, the headless content thread, the project's own frame
//! rate and output size, from the clip's start. By default it uses production
//! pacing and reports the wall interval between ticks and the GPU surface
//! wait. `--frame-clock` advances exactly one project frame per tick without
//! real-time pacing and waits for GPU completion after every tick, including
//! coupled reactions. Its wall sample is tick plus GPU fence, never window
//! FPS. Plain GPU time is the sum of the Generators and Compositor command-
//! buffer spans, each from its first chunk's start to its last chunk's end.
//! Every `K`th frame also carries per-dispatch GPU timestamps
//! split per node type, per dispatch label inside the solver and the
//! whitewater, and per pass label inside render_scene. Timestamped frames
//! open one encoder per dispatch with encode replay off, so their split is a
//! ratio, never the budget. `--stamp-granularity node` keeps one sampled
//! encoder per graph step instead (replay still off; inner labels are grouped).
//! A build with `manifold-renderer/water-race-probes` uses the solver's
//! existing stage tags to subdivide GPU FLIP preparation, solves and movement.
//! In paced mode a timestamped frame waits for its GPU work, a plain frame
//! does not, and
//! a liquid coupled to a body runs no tick while the last tick's reaction
//! is in flight: with plain and timestamped frames interleaved on such a
//! project the timestamped frames skip the step once the plain frame's GPU
//! work outlasts the tick interval (every one of them at 128). Measure a
//! coupled project with `--stamp-every 1` or use `--frame-clock`.
//! The first `S` frames are the splash, the rest
//! the calm; both tables print. `--resolution` and `--solve-level` override
//! the GPU FLIP generator's `resolution` and `solve_level` cards in memory,
//! never on disk; `--sim-rate` overrides the project's Sim Rate the same way.
//! Presentation is not timed here (no window); the pacing doc's vsync
//! quantisation applies on top of these numbers.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use manifold_gpu::{GpuFrameProfile, GpuWorkKind, ProfileGranularity};
use manifold_renderer::node_graph::StepProfile;

use crate::content_command::ContentCommand;
use crate::perf_soak::{prepare_project_edited, PreparedProject};

const GENERATOR: &str = "WaterDamBreakGpuFlip";
const RESOLUTION_PARAM: &str = "resolution";
const SOLVE_LEVEL_PARAM: &str = "solve_level";
const STEP: &str = "node.gpu_flip_step";
const WHITEWATER: &str = "node.whitewater_step";
const RENDER: &str = "node.render_scene";
/// Spans a timestamped frame may hold: the most a process can sample (32
/// buffers of 2,048 on M4 Max). A calm res-64 GPU FLIP frame stamps ~38,000.
const MAX_SPANS: usize = 65536;

struct Args {
    project: String,
    frames: usize,
    frame_clock: bool,
    resolution: Option<f32>,
    solve_level: Option<f32>,
    sim_rate: Option<manifold_core::settings::SimRate>,
    stamp_every: usize,
    granularity: ProfileGranularity,
    splash_frames: usize,
    png: Option<(usize, String)>,
}

fn usage_exit(msg: &str) -> ! {
    eprintln!("frame-time: {msg}");
    eprintln!(
        "usage: manifold frame-time <project.manifold> --frames N [--frame-clock] [--resolution R] [--solve-level L] [--sim-rate 15|20|30|60] \
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
    let frame_clock = args.iter().any(|arg| arg == "--frame-clock");
    let stamp_every = number("--stamp-every", Some(5)).max(1);
    let granularity = granularity(value(args, "--stamp-granularity").as_deref()).unwrap_or_else(|e| usage_exit(&e));
    let splash_frames = number("--splash-frames", Some(frames / 2));
    let resolution = value(args, "--resolution")
        .map(|s| s.parse::<f32>().unwrap_or_else(|_| usage_exit("--resolution must be a number")));
    let solve_level = value(args, "--solve-level")
        .map(|s| s.parse::<f32>().unwrap_or_else(|_| usage_exit("--solve-level must be a number")));
    let sim_rate = value(args, "--sim-rate").map(|s| {
        s.parse::<u32>().ok().and_then(|hz| manifold_core::settings::SimRate::try_from(hz).ok())
            .unwrap_or_else(|| usage_exit("--sim-rate must be 15, 20, 30 or 60"))
    });
    let png = match (value(args, "--png-frame"), value(args, "--png")) {
        (None, None) => None,
        (Some(_), None) | (None, Some(_)) => usage_exit("--png-frame and --png go together"),
        (Some(_), Some(path)) => Some((number("--png-frame", None), path)),
    };
    Args { project, frames, frame_clock, resolution, solve_level, sim_rate, stamp_every, granularity, splash_frames, png }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_clock_flag_defaults_off_and_parses() {
        let mut args: Vec<String> = ["frame-time", "water.manifold", "--frames", "2"].into_iter().map(str::to_owned).collect();
        assert!(!parse(&args).frame_clock);
        args.push("--frame-clock".into());
        assert!(parse(&args).frame_clock);
    }

    #[test]
    fn stamp_granularity_flag_parses() {
        assert_eq!(granularity(None), Ok(ProfileGranularity::Dispatch));
        assert_eq!(granularity(Some("dispatch")), Ok(ProfileGranularity::Dispatch));
        assert_eq!(granularity(Some("node")), Ok(ProfileGranularity::Tag));
        assert!(granularity(Some("step")).is_err());
    }

    #[test]
    fn explicit_flip_stage_tags_keep_their_solver_attribution() {
        let span = manifold_gpu::GpuProfiledSpan {
            tag: "gpu_flip.stage.pressure".into(),
            label: "first dispatch in grouped encoder".into(),
            kind: GpuWorkKind::Compute,
            millis: 2.5,
            start_ms: 0.0,
            threadgroup_bytes: 0,
        };
        let profiles = [("Generators", GpuFrameProfile { spans: vec![span], ..GpuFrameProfile::default() })];
        let split = split_profiles(&[], &profiles, ProfileGranularity::Tag);
        assert_eq!(split.per_type[STEP], 2.5);
        assert_eq!(split.per_step_label["gpu_flip.stage.pressure"], 2.5);
        assert!(split.compute_dispatches.is_none(), "a stage span is not one dispatch");
    }

    #[test]
    fn duplicate_profile_tags_sum_cpu_preparation_and_only_dispatch_mode_counts_compute() {
        use manifold_gpu::GpuProfiledSpan;
        use manifold_renderer::node_graph::NodeInstanceId;

        let steps: Vec<_> = [1_000_000, 2_000_000].into_iter().map(|cpu_nanos| StepProfile {
            step_idx: 0,
            node: NodeInstanceId(1),
            type_id: STEP.into(),
            cpu_nanos,
            tag: "water:s0".into(),
        }).collect();
        let spans = [GpuWorkKind::Compute, GpuWorkKind::Compute, GpuWorkKind::Blit].into_iter().map(|kind| GpuProfiledSpan {
            tag: "water:s0".into(),
            label: "pass".into(),
            kind,
            threadgroup_bytes: 0,
            start_ms: 0.0,
            millis: 0.5,
        }).collect();
        let profiles = [("Generators", GpuFrameProfile { total_ms: 2.0, spans, invalid: 1, ..GpuFrameProfile::default() })];
        let dispatch = split_profiles(&steps, &profiles, ProfileGranularity::Dispatch);
        assert_eq!(dispatch.per_type_cpu_ms[STEP], 3.0);
        assert_eq!(dispatch.per_type[STEP], 1.5);
        assert_eq!(dispatch.compute_dispatches.as_ref().unwrap()[STEP], 2);
        assert_eq!(dispatch.invalid, 1);
        let mut phase = Phase::default();
        phase.add(&Frame { interval_ms: 4.0, fence_ms: 0.0, plain_gpu_ms: None, split: Some(dispatch) });
        assert_eq!(phase.per_type_cpu_ms[STEP], vec![3.0]);
        assert_eq!(phase.compute_dispatches[STEP], vec![2.0]);
        assert_eq!(phase.dispatch_frames, 1);
        assert_eq!(phase.invalid, 1);
        let idle = split_profiles(&steps, &[], ProfileGranularity::Dispatch);
        phase.add(&Frame { interval_ms: 4.0, fence_ms: 0.0, plain_gpu_ms: None, split: Some(idle) });
        assert_eq!(phase.compute_dispatches[STEP], vec![2.0, 0.0]);
        assert_eq!(phase.dispatch_frames, 2);

        let tagged = split_profiles(&steps, &profiles, ProfileGranularity::Tag);
        assert_eq!(tagged.per_type_cpu_ms[STEP], 3.0);
        assert_eq!(tagged.per_type[STEP], 1.5);
        assert!(tagged.compute_dispatches.is_none());
        let mut phase = Phase::default();
        phase.add(&Frame { interval_ms: 4.0, fence_ms: 0.0, plain_gpu_ms: None, split: Some(tagged) });
        assert_eq!(phase.per_type_cpu_ms[STEP], vec![3.0]);
        assert!(phase.compute_dispatches.is_empty());
        assert_eq!(phase.dispatch_frames, 0);
    }
}

/// One frame's numbers: paced tick interval/surface wait, or fixed-clock
/// tick plus fence/completion wait. A timestamped frame also has its split.
struct Frame {
    interval_ms: f64,
    fence_ms: f64,
    /// Plain frames only: Generators and Compositor command-buffer spans,
    /// from their completion handlers, including gaps between their chunks.
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
    invalid: usize,
    per_type: BTreeMap<String, f64>,
    /// Profiled graph-step preparation: acquire, encode and scalar drains.
    per_type_cpu_ms: BTreeMap<String, f64>,
    /// Tag-granularity compute spans can contain multiple dispatches.
    compute_dispatches: Option<BTreeMap<String, u64>>,
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
    invalid: usize,
    per_type: BTreeMap<String, Vec<f64>>,
    per_type_cpu_ms: BTreeMap<String, Vec<f64>>,
    compute_dispatches: BTreeMap<String, Vec<f64>>,
    dispatch_frames: usize,
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
        self.invalid += split.invalid;
        for (into, from) in [
            (&mut self.per_type, &split.per_type),
            (&mut self.per_type_cpu_ms, &split.per_type_cpu_ms),
            (&mut self.per_step, &split.per_step_label),
            (&mut self.per_whitewater, &split.per_whitewater_label),
            (&mut self.per_render, &split.per_render_label),
        ] {
            for (name, ms) in from {
                into.entry(name.clone()).or_default().push(*ms);
            }
        }
        if let Some(counts) = &split.compute_dispatches {
            self.dispatch_frames += 1;
            // A type with no compute span in this sampled frame dispatched zero.
            for samples in self.compute_dispatches.values_mut() {
                samples.push(0.0);
            }
            for (name, count) in counts {
                let samples = self.compute_dispatches.entry(name.clone())
                    .or_insert_with(|| vec![0.0; self.dispatch_frames]);
                *samples.last_mut().expect("the current dispatch frame has a count slot") = *count as f64;
            }
        }
    }

    fn report(&self, name: &str, frame_clock: bool) {
        println!("== {name} ==");
        let (wall_label, wait_label, rate_note) = if frame_clock {
            ("tick + GPU fence", "post-tick GPU completion wait", String::new())
        } else {
            ("tick interval", "GPU surface wait", format!(" (fps p50 {:.1})", 1000.0 / percentile(&self.interval, 0.5)))
        };
        println!(
            "  frames ({}): {wall_label} p50 {:.2} ms p95 {:.2} ms{rate_note} | {wait_label} p50 {:.2} ms p95 {:.2} ms",
            self.interval.len(),
            percentile(&self.interval, 0.5),
            percentile(&self.interval, 0.95),
            percentile(&self.fence, 0.5),
            percentile(&self.fence, 0.95),
        );
        println!(
            "  plain frames ({}): command-buffer GPU spans, ms  sum p50 {:.2} p95 {:.2} max {:.2} | generators p50 {:.2} p95 {:.2} | compositor p50 {:.2} p95 {:.2}",
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
            "  timestamped frames ({}): whole-buffer GPU p50 {:.2} ms p95 {:.2} ms, sampler overflow {}, invalid spans {}",
            self.stamped_total.len(),
            percentile(&self.stamped_total, 0.5),
            percentile(&self.stamped_total, 0.95),
            self.overflow,
            self.invalid,
        );
        if self.overflow > 0 || self.invalid > 0 {
            println!(
                "  WARNING: {} spans did not fit the sampler and {} spans had invalid samples; GPU span timing and dispatch-count tables below are incomplete",
                self.overflow,
                self.invalid,
            );
        }
        print_split("per node type", &self.per_type);
        let (step_title, whitewater_title) = if self.dispatch_frames > 0 {
            ("gpu_flip_step per dispatch label", "whitewater_step per dispatch label")
        } else {
            ("gpu_flip_step grouped encoder labels", "whitewater_step grouped encoder labels")
        };
        print_split(step_title, &self.per_step);
        print_split(whitewater_title, &self.per_whitewater);
        print_split("render_scene per pass label", &self.per_render);
        println!("  profiled CPU preparation per node type (acquire, encode, scalar drains; timestamped frames):");
        let mut cpu_rows: Vec<_> = self.per_type_cpu_ms.iter().collect();
        cpu_rows.sort_by(|a, b| percentile(b.1, 0.5).total_cmp(&percentile(a.1, 0.5)));
        for (name, samples) in cpu_rows {
            println!("    {name:<56} p50 {:>8.3} ms  p95 {:>8.3} ms", percentile(samples, 0.5), percentile(samples, 0.95));
        }
        if self.dispatch_frames > 0 {
            println!("  sampled compute dispatch counts per node type ({} dispatch-granularity frames):", self.dispatch_frames);
            let mut count_rows: Vec<_> = self.compute_dispatches.iter().collect();
            count_rows.sort_by(|a, b| percentile(b.1, 0.5).total_cmp(&percentile(a.1, 0.5)));
            for (name, samples) in count_rows {
                println!("    {name:<56} p50 {:>8.0} dispatches  p95 {:>8.0} dispatches", percentile(samples, 0.5), percentile(samples, 0.95));
            }
        }
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
fn split(ct: &mut crate::content_thread::ContentThread, granularity: ProfileGranularity) -> Split {
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
    split_profiles(&steps, &gpu_profiles, granularity)
}

fn split_profiles(
    steps: &[StepProfile],
    gpu_profiles: &[(&str, GpuFrameProfile)],
    granularity: ProfileGranularity,
) -> Split {
    let mut split = Split {
        compute_dispatches: (granularity == ProfileGranularity::Dispatch).then(BTreeMap::new),
        ..Split::default()
    };
    let mut types = BTreeMap::new();
    for step in steps {
        *split.per_type_cpu_ms.entry(step.type_id.clone()).or_insert(0.0) += step.cpu_nanos as f64 / 1e6;
        types.insert(step.tag.as_str(), step.type_id.as_str());
    }
    for (buffer, profile) in gpu_profiles {
        split.total_ms += profile.total_ms;
        split.overflow += profile.overflow;
        split.invalid += profile.invalid;
        for span in &profile.spans {
            // The opt-in water-race-probes build subdivides GPU FLIP's node
            // tag into stages. Preserve those labels in real-project probes.
            let stage = span.tag.starts_with("gpu_flip.stage.");
            let type_id = if stage { STEP.to_owned() } else {
                types.get(span.tag.as_str()).map_or_else(|| format!("(untagged, {buffer})"), |type_id| (*type_id).to_owned())
            };
            *split.per_type.entry(type_id.clone()).or_insert(0.0) += span.millis;
            if span.kind == GpuWorkKind::Compute && let Some(counts) = &mut split.compute_dispatches {
                *counts.entry(type_id.clone()).or_insert(0) += 1;
            }
            let labelled = match type_id.as_str() {
                STEP => Some(&mut split.per_step_label),
                WHITEWATER => Some(&mut split.per_whitewater_label),
                RENDER => Some(&mut split.per_render_label),
                _ => None,
            };
            if let Some(labelled) = labelled {
                let key = if stage { span.tag.clone() } else if type_id == RENDER { format!("{:?} {}", span.kind, span.label) } else { span.label.clone() };
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
    let initial_faults = manifold_gpu::gpu_fault::fault_count();
    let mut overridden = 0usize;
    let mut sim_rate = manifold_core::settings::SimRate::default();
    let PreparedProject { mut ct, cmd_tx, cmd_rx, state_tx, drain, width, height, frame_rate, .. } =
        prepare_project_edited(&args.project, "frame-time", &mut |project| {
            if let Some(rate) = args.sim_rate {
                project.settings.physics.sim_rate = rate;
            }
            sim_rate = project.settings.physics.sim_rate;
            let overrides = [(RESOLUTION_PARAM, args.resolution), (SOLVE_LEVEL_PARAM, args.solve_level)];
            for layer in &mut project.timeline.layers {
                if !layer.hosts_generator() || layer.generator_type().as_str() != GENERATOR {
                    continue;
                }
                let Some(params) = layer.gen_params_mut() else { continue };
                for (name, value) in overrides {
                    if let Some(value) = value
                        && params.set_base_param(name, value)
                    {
                        overridden += 1;
                    }
                }
            }
        })?;
    let asked = usize::from(args.resolution.is_some()) + usize::from(args.solve_level.is_some());
    if asked > 0 && overridden < asked {
        return Err(format!("a {GENERATOR} layer took {overridden} of the {asked} overrides (resolution, solve level)"));
    }
    let resolution_note = args.resolution.map_or("the file's resolution".to_owned(), |r| format!("resolution {r}"))
        + &args.solve_level.map_or(String::new(), |l| format!(", solve level {l}"))
        + &format!(", sim rate {} Hz", sim_rate.hz());
    let device_name = ct.content_pipeline.native_device().map_or("unknown".to_owned(), |d| d.device_name());
    let mode = if args.frame_clock {
        "frame clock, no pacing, completion wait every frame; wall = tick + GPU fence"
    } else {
        "real-time paced; wall = tick interval"
    };
    println!(
        "frame-time: {mode}; {} at {width}x{height} @ {frame_rate} project fps, {resolution_note}, {} frames, every {}th timestamped per {}, splash = first {} frames, on {device_name}",
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

    ct.timer.set_frame_clocked(args.frame_clock);
    ct.timer.resume_after_load();
    ct.handle_command(ContentCommand::Play);
    let (gpu_time_tx, gpu_time_rx) = crossbeam_channel::unbounded::<(&'static str, f64, f64)>();
    ct.content_pipeline.set_gpu_time_tap(Some(gpu_time_tx));
    let mut frames: Vec<Frame> = Vec::with_capacity(args.frames);
    let mut last = Instant::now();
    for index in 0..args.frames {
        let stamped = index % args.stamp_every == args.stamp_every - 1;
        set_profiling(&mut ct, stamped);
        let (interval_ms, fence_ms) = if args.frame_clock {
            let start = Instant::now();
            ct.timer.ensure_thread_policy();
            objc2::rc::autoreleasepool(|_| ct.tick_frame(&state_tx));
            let fence_start = Instant::now();
            ct.content_pipeline.wait_for_render_complete();
            // The live waiter may time out or wake early. Reuse the existing
            // checked fence before accepting an equal-progress sample.
            ct.content_pipeline.wait_for_export_complete(initial_faults)?;
            let fence_ms = fence_start.elapsed().as_secs_f64() * 1e3;
            (start.elapsed().as_secs_f64() * 1e3, fence_ms)
        } else {
            if ct.run_paced_frame(&cmd_tx, &cmd_rx, &state_tx) {
                return Err("shutdown requested mid-run".into());
            }
            let now = Instant::now();
            let interval_ms = now.duration_since(last).as_secs_f64() * 1e3;
            last = now;
            (interval_ms, ct.content_pipeline.last_fence_wait_ms())
        };
        let frame_split = stamped.then(|| split(&mut ct, args.granularity));
        if args.frame_clock && let Some(split) = &frame_split
            && (split.invalid != 0 || split.overflow != 0)
        {
            return Err(format!("frame {index}: incomplete GPU attribution ({} invalid, {} overflow)", split.invalid, split.overflow));
        }
        if let Some((at, path)) = &args.png
            && *at == index
        {
            write_png(&mut ct, path);
        }
        frames.push(Frame { interval_ms, fence_ms, plain_gpu_ms: None, split: frame_split });
    }
    set_profiling(&mut ct, false);
    ct.content_pipeline.wait_for_render_complete();
    ct.content_pipeline.set_gpu_time_tap(None);
    drop(state_tx);
    drain.join().map_err(|_| "drain thread panicked".to_string())?;

    // Completion handlers fire in submission order; the plain frames took
    // them in that same order. A frame is every Generators chunk, then every
    // Compositor chunk (chunking splits each encoder into several buffers);
    // a Generators chunk after a Compositor one starts the next frame. The
    // chunks of each encoder overlap on the GPU, so its time is the span
    // from its first start to its last end, not the sum of chunk durations.
    // The reported plain sum adds the two encoder spans. Allow completion
    // handlers to deliver the final taps before grouping.
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
        if args.frame_clock {
            return Err(format!("{unpaired} fixed-clock frames got no GPU completion time"));
        }
        println!("  WARNING: {unpaired} plain frames got no GPU completion time");
    }

    let wall_label = if args.frame_clock { "tick + GPU fence" } else { "wall interval" };
    println!("  per frame: {wall_label} ms / command-buffer GPU span sum ms (* = timestamped, replay off, no plain GPU time):");
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
    whole.report("whole run", args.frame_clock);
    splash.report(&format!("splash (frames 0..{})", args.splash_frames), args.frame_clock);
    calm.report(&format!("calm (frames {}..{})", args.splash_frames, args.frames), args.frame_clock);
    Ok(())
}
