//! Shader text swaps for `manifold look-loop`: an edited shader file reaches
//! the next pipeline compile without a cargo build.
//!
//! Shaders are built into the binary (`include_str!`), so the loop hands over
//! each edited file's built-in text and its edited text, and every pipeline
//! source containing the built-in text compiles with the edited text instead.
//! Swaps apply before a pipeline's text specialisations, whose patterns match
//! the built-in text. Metal only. Off unless the loop turns it on; a compile
//! then pays one relaxed load.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ON: AtomicBool = AtomicBool::new(false);
static STATE: Mutex<Option<State>> = Mutex::new(None);

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

#[derive(Default)]
struct State {
    swaps: Vec<Swap>,
    /// Every distinct source compiled since `enable`, before swaps, with the
    /// first specialisation set it compiled under: what [`set`] checks an
    /// edit against before any pipeline compiles it.
    seen: HashMap<u64, Seen>,
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

/// `source` with every swap applied, or `None` when swaps are off or none
/// lands in it. Remembers `source` for [`set`]'s check.
pub(crate) fn apply(source: &str, specializations: &[(&str, &str)]) -> Option<String> {
    if !ON.load(Ordering::Relaxed) {
        return None;
    }
    let mut guard = STATE.lock().expect("shader swaps poisoned");
    let state = guard.as_mut()?;
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    state.seen.entry(hasher.finish()).or_insert_with(|| Seen {
        source: source.to_owned(),
        specializations: specializations
            .iter()
            .map(|&(pattern, replacement)| (pattern.to_owned(), replacement.to_owned()))
            .collect(),
    });
    swapped(source, &state.swaps)
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
