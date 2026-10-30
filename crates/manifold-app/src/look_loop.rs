//! `manifold look-loop <project.manifold> --frame F --png <out.png>
//! [--params <file.json>] [--watch <dir>]... [--settle K]` — a warm still
//! for look work. Loads the project the way `frame-time --frame-clock` does,
//! plays to frame F (numbered as `--png-frame F` numbers it), pauses, renders
//! K paused frames so temporal passes settle, and writes the PNG. Then every
//! `render` line on stdin re-reads the watched `.wgsl` files and the params
//! file and writes the PNG again from the same paused state: no cargo build,
//! no re-simulation.
//!
//! An edited shader file swaps its built-in text at compile time
//! (`manifold_gpu::shader_swap`, checked with naga first), the pipelines the
//! render node drew with compile again in parallel, and every
//! `node.render_scene` gets a fresh instance that finds them compiled; the
//! simulation nodes keep their state. The params file is a flat JSON object
//! of generator card param ids to numbers, written over the project's values
//! the way a slider writes them; an id the file drops goes back to the
//! project's value. Look params only: a simulation param changed here does
//! not re-simulate. Every reply is one stdout line starting `look-loop: `
//! (`ready`, `done` or `error`). `scripts/look_loop.py` drives it and holds
//! the GPU lock only around start-up and each render.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::BufRead;
use std::path::PathBuf;
use std::time::Instant;

use manifold_core::LayerId;
use manifold_gpu::shader_swap::{self, Swap};

use crate::content_command::{ContentCommand, touch};
use crate::content_state::StateTx;
use crate::content_thread::ContentThread;
use crate::frame_time::{settle_loads, write_png};
use crate::perf_soak::{PreparedProject, prepare_project_edited};

/// The node that compiles the scene's look shaders.
const RENDER_NODE: &str = "node.render_scene";

struct Args {
    project: String,
    frame: usize,
    png: String,
    params: Option<PathBuf>,
    watch: Vec<PathBuf>,
    settle: usize,
}

fn usage_exit(msg: &str) -> ! {
    eprintln!("look-loop: {msg}");
    eprintln!(
        "usage: manifold look-loop <project.manifold> --frame F --png <out.png> [--params <file.json>] [--watch <dir>]... [--settle K]"
    );
    std::process::exit(2);
}

fn parse(args: &[String]) -> Args {
    let project = match args.get(1) {
        Some(p) if !p.starts_with("--") => p.clone(),
        _ => usage_exit("missing <project.manifold> argument"),
    };
    let value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let number = |flag: &str, default: Option<usize>| -> usize {
        match value(flag) {
            Some(s) => s
                .parse()
                .unwrap_or_else(|_| usage_exit(&format!("{flag} must be a whole number"))),
            None => default.unwrap_or_else(|| usage_exit(&format!("{flag} is required"))),
        }
    };
    let watch = args
        .windows(2)
        .filter(|pair| pair[0] == "--watch")
        .map(|pair| PathBuf::from(&pair[1]))
        .collect();
    Args {
        frame: number("--frame", None),
        png: value("--png").unwrap_or_else(|| usage_exit("--png is required")),
        params: value("--params").map(PathBuf::from),
        settle: number("--settle", Some(8)).max(1),
        project,
        watch,
    }
}

pub fn run(args: &[String]) -> ! {
    let _ = env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn"))
        .try_init();
    let args = parse(args);
    match look_loop(&args) {
        Ok(()) => std::process::exit(0),
        Err(e) => {
            println!("look-loop: error {e}");
            std::process::exit(3);
        }
    }
}

/// The text of every `.wgsl` file under the watched directories, by path.
fn read_shaders(dirs: &[PathBuf]) -> Result<BTreeMap<PathBuf, String>, String> {
    let mut texts = BTreeMap::new();
    for dir in dirs {
        let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for entry in entries {
            let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
            if path.extension().is_some_and(|ext| ext == "wgsl") {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                texts.insert(path, text);
            }
        }
    }
    Ok(texts)
}

/// The params file's values over the project's own.
#[derive(Default)]
struct Overrides {
    /// What the file set last, by param id.
    applied: BTreeMap<String, f32>,
    /// Each overridden param's project value, by layer and param id.
    original: HashMap<(LayerId, String), f32>,
}

fn read_params(path: &PathBuf) -> Result<BTreeMap<String, f32>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let values: BTreeMap<String, f64> =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(values.into_iter().map(|(id, v)| (id, v as f32)).collect())
}

/// Write the params that changed, through the same live value write a
/// slider uses, and put back the ones the file dropped.
fn apply_params(
    ct: &mut ContentThread,
    overrides: &mut Overrides,
    wanted: BTreeMap<String, f32>,
) -> Result<(), String> {
    let restores: Vec<(LayerId, String, f32)> = overrides
        .original
        .iter()
        .filter(|((_, id), _)| !wanted.contains_key(id))
        .map(|((layer, id), value)| (layer.clone(), id.clone(), *value))
        .collect();
    let sets: Vec<(String, f32)> = wanted
        .iter()
        .filter(|(id, value)| overrides.applied.get(*id) != Some(*value))
        .map(|(id, value)| (id.clone(), *value))
        .collect();
    if restores.is_empty() && sets.is_empty() {
        return Ok(());
    }
    let known: HashSet<(LayerId, String)> = overrides.original.keys().cloned().collect();
    let (tx, rx) = std::sync::mpsc::channel();
    let write_restores = restores.clone();
    ct.handle_command(ContentCommand::mutate(
        touch::VALUES,
        Box::new(move |project| {
            let mut firsts = Vec::new();
            let mut hits: BTreeMap<String, usize> =
                sets.iter().map(|(id, _)| (id.clone(), 0)).collect();
            for layer in &mut project.timeline.layers {
                let layer_id = layer.layer_id.clone();
                let Some(params) = layer.gen_params_mut() else {
                    continue;
                };
                for (_, id, value) in write_restores.iter().filter(|r| r.0 == layer_id) {
                    params.set_base_param(id, *value);
                }
                for (id, value) in &sets {
                    let before = params.get_base_param(id);
                    if params.set_base_param(id, *value) {
                        *hits.entry(id.clone()).or_default() += 1;
                        let key = (layer_id.clone(), id.clone());
                        if !known.contains(&key) {
                            firsts.push((key, before));
                        }
                    }
                }
            }
            let _ = tx.send((firsts, hits));
        }),
    ));
    let (firsts, hits) = rx.recv().map_err(|_| "the params write never ran")?;
    for (layer, id, _) in restores {
        overrides.original.remove(&(layer, id));
    }
    overrides.original.extend(firsts);
    overrides.applied = wanted;
    let missing: Vec<String> = hits
        .into_iter()
        .filter(|(_, layers)| *layers == 0)
        .map(|(id, _)| id)
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "no generator layer has param {}",
            missing.join(", ")
        ))
    }
}

/// One frame on the frame clock, waited for on the GPU.
fn tick(
    ct: &mut ContentThread,
    state_tx: &StateTx,
    faults: &manifold_gpu::gpu_fault::FaultMark,
) -> Result<(), String> {
    ct.timer.ensure_thread_policy();
    manifold_gpu::autorelease_scope(|| ct.tick_frame(state_tx));
    ct.content_pipeline.wait_for_render_complete();
    ct.content_pipeline
        .wait_for_export_complete(faults)
        .map_err(|failure| failure.message)
}

fn rebuild_render_nodes(ct: &mut ContentThread) -> usize {
    ct.engine
        .renderers_mut()
        .iter_mut()
        .filter_map(|renderer| {
            renderer
                .as_any_mut()
                .downcast_mut::<manifold_compositor::generator_renderer::GeneratorRenderer>()
        })
        .map(|generator| generator.rebuild_nodes_of_type(RENDER_NODE))
        .sum()
}

struct Loop {
    ct: ContentThread,
    state_tx: StateTx,
    faults: manifold_gpu::gpu_fault::FaultMark,
    built: BTreeMap<PathBuf, String>,
    swaps: Vec<Swap>,
    overrides: Overrides,
    /// Whether a fresh render node has drawn with its pipelines recorded.
    recorded: bool,
}

impl Loop {
    /// Re-read the shaders and the params, then render. Notes say what an
    /// edit did not reach.
    fn render(&mut self, args: &Args) -> Result<Vec<String>, String> {
        let mut notes = Vec::new();
        let now = read_shaders(&args.watch)?;
        let swaps: Vec<Swap> = self
            .built
            .iter()
            .filter_map(|(path, built)| {
                let edited = now.get(path).filter(|text| *text != built)?;
                Some(Swap {
                    file: path.display().to_string(),
                    built: built.clone(),
                    edited: edited.clone(),
                })
            })
            .collect();
        // The first render rebuilds too, so the node's pipelines get recorded.
        let mut rebuild = !self.recorded;
        if swaps != self.swaps {
            let landed = shader_swap::set(swaps.clone())?;
            self.swaps = swaps;
            if landed == 0 {
                notes.push(
                    "the shader edit is in no shader this scene compiled (unused here, or the binary is older than the file)"
                        .to_owned(),
                );
            }
            // The pipelines the last fresh node drew with, compiled in
            // parallel before the next one asks for them one by one.
            if let Some(device) = self.ct.content_pipeline.native_device() {
                shader_swap::compile_recorded(device);
            }
            rebuild = true;
        }
        if rebuild {
            if rebuild_render_nodes(&mut self.ct) == 0 {
                notes.push(format!("no {RENDER_NODE} to rebuild"));
            }
            shader_swap::record(true);
        }
        if let Some(path) = &args.params {
            let wanted = read_params(path)?;
            apply_params(&mut self.ct, &mut self.overrides, wanted)?;
        }
        let ticked =
            (0..args.settle).try_for_each(|_| tick(&mut self.ct, &self.state_tx, &self.faults));
        shader_swap::record(false);
        ticked?;
        self.recorded |= rebuild;
        write_png(&mut self.ct, &args.png)?;
        Ok(notes)
    }
}

fn look_loop(args: &Args) -> Result<(), String> {
    let started = Instant::now();
    // Before the first compile, so every shader source is seen.
    shader_swap::enable();
    let built = read_shaders(&args.watch)?;
    let faults = manifold_gpu::gpu_fault::mark();
    let PreparedProject {
        mut ct,
        cmd_tx: _cmd_tx,
        cmd_rx: _cmd_rx,
        state_tx,
        drain,
        ..
    } = prepare_project_edited(&args.project, "look-loop", &mut |_| {})?;
    ct.timer.set_frame_clocked(true);
    ct.timer.resume_after_load();
    settle_loads(&mut ct, &state_tx)?;
    let mut overrides = Overrides::default();
    if let Some(path) = &args.params {
        apply_params(&mut ct, &mut overrides, read_params(path)?)?;
    }
    ct.handle_command(ContentCommand::Play);
    for _ in 0..=args.frame {
        tick(&mut ct, &state_tx, &faults)?;
    }
    ct.handle_command(ContentCommand::Pause);
    let mut warm = Loop {
        ct,
        state_tx,
        faults,
        built,
        swaps: Vec::new(),
        overrides,
        recorded: false,
    };
    warm.render(args)?;
    println!(
        "look-loop: ready {} at frame {} in {:.1} s, watching {} shaders",
        args.png,
        args.frame,
        started.elapsed().as_secs_f64(),
        warm.built.len()
    );
    for line in std::io::stdin().lock().lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        match line.trim() {
            "render" => {
                let started = Instant::now();
                match warm.render(args) {
                    Ok(notes) => println!(
                        "look-loop: done {} in {:.2} s{}",
                        args.png,
                        started.elapsed().as_secs_f64(),
                        notes
                            .iter()
                            .map(|note| format!("; {note}"))
                            .collect::<String>()
                    ),
                    // One stdout line per reply; naga's full message, with
                    // the offending line, goes to stderr.
                    Err(e) => {
                        eprintln!("{e}");
                        println!("look-loop: error {}", e.lines().next().unwrap_or_default());
                    }
                }
            }
            "quit" => break,
            other => println!("look-loop: error unknown command {other:?}"),
        }
    }
    drop(warm.state_tx);
    drain
        .join()
        .map_err(|_| "drain thread panicked".to_string())
}
