//! Shader text swaps for `manifold look-loop`: an edited shader file reaches
//! the next pipeline compile without a cargo build.
//!
//! Shaders are built into the binary (`include_str!`), so the loop hands over
//! each edited file's built-in text and its edited text, and every pipeline
//! source containing the built-in text compiles with the edited text instead.
//! Swaps apply before a pipeline's text specialisations, whose patterns match
//! the built-in text. The pipelines a fresh render node asks for are recorded,
//! so after an edit they compile in parallel before the node asks again.
//! Metal only. Off unless the loop turns it on; a compile then pays one
//! relaxed load.

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::{GpuBlendState, GpuDevice, GpuTextureFormat, GpuVertexLayout, RenderPipelineDesc};

static ON: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

/// Parallel compiles after an edit: the start-up warm-up's cap
/// (`RenderScene::prewarm_pipelines`), since a cold scene-shader compile
/// holds over 2 GB inside spirv-opt.
const WORKERS: usize = 4;

/// One shader file edited away from the text built into the binary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Swap {
    pub file: String,
    pub built: String,
    pub edited: String,
}

struct Seen {
    source: String,
    specializations: Vec<(String, String)>,
}

/// One pipeline as it was asked for, before swaps; `source` keys `seen`.
#[derive(Clone, Hash)]
enum Recipe {
    Render {
        source: u64,
        vs_entry: String,
        fs_entry: String,
        label: String,
        color_format: Option<GpuTextureFormat>,
        blend: Option<GpuBlendState>,
        aux_color_formats: Vec<GpuTextureFormat>,
        depth_format: Option<GpuTextureFormat>,
        sample_count: u32,
        alpha_to_coverage: bool,
        point_size_location: Option<u32>,
        vertex_layout: Option<GpuVertexLayout>,
        specializations: Vec<(String, String)>,
    },
    Compute {
        source: u64,
        entry: String,
        label: String,
        specializations: Vec<(String, String)>,
    },
}

impl Recipe {
    fn source(&self) -> u64 {
        match self {
            Recipe::Render { source, .. } | Recipe::Compute { source, .. } => *source,
        }
    }

    fn compile(&self, device: &GpuDevice, wgsl_source: &str) {
        match self {
            Recipe::Render {
                vs_entry,
                fs_entry,
                label,
                color_format,
                blend,
                aux_color_formats,
                depth_format,
                sample_count,
                alpha_to_coverage,
                point_size_location,
                vertex_layout,
                specializations,
                ..
            } => {
                let specializations = borrowed(specializations);
                device.create_render_pipeline(&RenderPipelineDesc {
                    wgsl_source,
                    vs_entry,
                    fs_entry,
                    label,
                    color_format: *color_format,
                    blend: *blend,
                    aux_color_formats,
                    depth_format: *depth_format,
                    sample_count: *sample_count,
                    alpha_to_coverage: *alpha_to_coverage,
                    point_size_location: *point_size_location,
                    vertex_layout: vertex_layout.as_ref(),
                    specializations: &specializations,
                });
            }
            Recipe::Compute {
                entry,
                label,
                specializations,
                ..
            } => {
                if specializations.is_empty() {
                    device.create_compute_pipeline(wgsl_source, entry, label);
                } else {
                    let specializations = borrowed(specializations);
                    device.create_specialized_compute_pipeline(
                        wgsl_source,
                        entry,
                        &specializations,
                        label,
                    );
                }
            }
        }
    }
}

fn owned(specializations: &[(&str, &str)]) -> Vec<(String, String)> {
    specializations
        .iter()
        .map(|&(pattern, replacement)| (pattern.to_owned(), replacement.to_owned()))
        .collect()
}

fn borrowed(specializations: &[(String, String)]) -> Vec<(&str, &str)> {
    specializations
        .iter()
        .map(|(pattern, replacement)| (pattern.as_str(), replacement.as_str()))
        .collect()
}

fn hash_of(value: &impl Hash) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct State {
    swaps: Vec<Swap>,
    /// Every distinct source compiled since `enable`, before swaps, with the
    /// first specialisation set it compiled under: what [`set`] checks an
    /// edit against before any pipeline compiles it.
    seen: HashMap<u64, Seen>,
    recording: bool,
    recorded: Vec<Recipe>,
    recorded_keys: HashSet<u64>,
}

impl State {
    fn see(&mut self, source: &str, specializations: &[(&str, &str)]) -> u64 {
        let key = hash_of(&source);
        self.seen.entry(key).or_insert_with(|| Seen {
            source: source.to_owned(),
            specializations: owned(specializations),
        });
        key
    }

    fn record(&mut self, recipe: impl FnOnce() -> Recipe) {
        if !self.recording {
            return;
        }
        let recipe = recipe();
        if self.recorded_keys.insert(hash_of(&recipe)) {
            self.recorded.push(recipe);
        }
    }
}

/// Turn swaps on. Call before the first pipeline compiles, so every source
/// is seen.
pub fn enable() {
    STATE
        .lock()
        .expect("shader swaps poisoned")
        .get_or_insert_with(State::default);
    ON.store(true, Ordering::Relaxed);
}

/// The source a render pipeline compiles: `None` when swaps are off or none
/// lands in `desc`'s source, else the swapped text.
pub(crate) fn apply_render(desc: &RenderPipelineDesc) -> Option<String> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    let mut guard = STATE.lock().expect("shader swaps poisoned");
    let state = guard.as_mut()?;
    let source = state.see(desc.wgsl_source, desc.specializations);
    state.record(|| Recipe::Render {
        source,
        vs_entry: desc.vs_entry.to_owned(),
        fs_entry: desc.fs_entry.to_owned(),
        label: desc.label.to_owned(),
        color_format: desc.color_format,
        blend: desc.blend,
        aux_color_formats: desc.aux_color_formats.to_vec(),
        depth_format: desc.depth_format,
        sample_count: desc.sample_count,
        alpha_to_coverage: desc.alpha_to_coverage,
        point_size_location: desc.point_size_location,
        vertex_layout: desc.vertex_layout.cloned(),
        specializations: owned(desc.specializations),
    });
    swapped(desc.wgsl_source, &state.swaps)
}

/// [`apply_render`] for a compute pipeline.
pub(crate) fn apply_compute(
    source: &str,
    entry: &str,
    label: &str,
    specializations: &[(&str, &str)],
) -> Option<String> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    let mut guard = STATE.lock().expect("shader swaps poisoned");
    let state = guard.as_mut()?;
    let key = state.see(source, specializations);
    state.record(|| Recipe::Compute {
        source: key,
        entry: entry.to_owned(),
        label: label.to_owned(),
        specializations: owned(specializations),
    });
    swapped(source, &state.swaps)
}

/// Start recording the pipelines asked for (dropping the last record), or
/// stop. The loop records while a freshly built render node draws.
pub fn record(on: bool) {
    let mut guard = STATE.lock().expect("shader swaps poisoned");
    let Some(state) = guard.as_mut() else { return };
    if on {
        state.recorded.clear();
        state.recorded_keys.clear();
    }
    state.recording = on;
}

/// Compile every recorded pipeline the swaps land in, [`WORKERS`] at a
/// time, so the next fresh render node finds each in the device cache
/// instead of compiling them one after another. Returns how many.
pub fn compile_recorded(device: &GpuDevice) -> usize {
    let jobs: Vec<(String, Recipe)> = {
        let guard = STATE.lock().expect("shader swaps poisoned");
        let Some(state) = guard.as_ref() else {
            return 0;
        };
        state
            .recorded
            .iter()
            .filter_map(|recipe| {
                let seen = state.seen.get(&recipe.source())?;
                swapped(&seen.source, &state.swaps)?;
                Some((seen.source.clone(), recipe.clone()))
            })
            .collect()
    };
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(jobs.len()) {
            scope.spawn(|| {
                while let Some((source, recipe)) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                    recipe.compile(device, source);
                }
            });
        }
    });
    jobs.len()
}

fn swapped(source: &str, swaps: &[Swap]) -> Option<String> {
    let mut out: Option<String> = None;
    for swap in swaps {
        let current = out.as_deref().unwrap_or(source);
        if current.contains(swap.built.as_str()) {
            out = Some(current.replace(swap.built.as_str(), &swap.edited));
        }
    }
    out
}

/// Replace the swap set with `swaps` (every file whose edited text differs
/// from its built-in text). Each seen shader the change lands in is checked
/// with naga first; a failed check changes nothing and returns naga's
/// message. Returns how many seen shaders the change lands in.
pub fn set(swaps: Vec<Swap>) -> Result<usize, String> {
    if let Some(empty) = swaps.iter().find(|swap| swap.built.trim().is_empty()) {
        return Err(format!(
            "{}: an empty file cannot be found in a shader",
            empty.file
        ));
    }
    let mut guard = STATE.lock().expect("shader swaps poisoned");
    let state = guard.as_mut().ok_or("shader swaps are off")?;
    let mut landed = 0;
    for seen in state.seen.values() {
        let after = swapped(&seen.source, &swaps);
        if after == swapped(&seen.source, &state.swaps) {
            continue;
        }
        landed += 1;
        // Back to the built-in text: it compiled before, nothing to check.
        let Some(mut text) = after else { continue };
        for (pattern, replacement) in &seen.specializations {
            text = text.replace(pattern.as_str(), replacement);
        }
        crate::shader_common::check_wgsl(&text)?;
    }
    state.swaps = swaps;
    Ok(landed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn swap(built: &str, edited: &str) -> Swap {
        Swap {
            file: "a.wgsl".into(),
            built: built.into(),
            edited: edited.into(),
        }
    }

    #[test]
    fn swaps_replace_every_copy_and_chain_in_order() {
        let source = "fn a() {}\nfn b() {}\nfn a() {}\n";
        assert_eq!(swapped(source, &[]), None);
        assert_eq!(swapped(source, &[swap("fn c() {}", "x")]), None);
        assert_eq!(
            swapped(
                source,
                &[
                    swap("fn a() {}", "fn a2() {}"),
                    swap("fn b() {}", "fn b2() {}")
                ]
            )
            .as_deref(),
            Some("fn a2() {}\nfn b2() {}\nfn a2() {}\n")
        );
    }
}
