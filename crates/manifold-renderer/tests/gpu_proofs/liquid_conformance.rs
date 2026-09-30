//! The liquid conformance suite on the GPU (`docs/LIQUID_SOLVER_SEAM_DESIGN.md`
//! section 4 (Invariants & enforcement), phase P4).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use manifold_core::effect_graph_def::EffectGraphNode;
use manifold_core::liquid_domain::is_liquid_domain;
use manifold_core::preset_def::PresetKind;
use manifold_renderer::node_graph::{bundled_preset_def, bundled_preset_type_ids};
use manifold_renderer::preset_thumbnail::{THUMBNAIL_HEIGHT, THUMBNAIL_WIDTH, render_preset_thumbnail};

use crate::harness;

/// Every core busy twice over while `f` runs.
fn contended<T>(f: impl FnOnce() -> T) -> T {
    let stop = Arc::new(AtomicBool::new(false));
    let threads = 2 * std::thread::available_parallelism().map_or(8, |n| n.get());
    let burners: Vec<_> = (0..threads)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut x = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    x = std::hint::black_box(x.wrapping_mul(6364136223846793005).wrapping_add(1));
                }
            })
        })
        .collect();
    let result = f();
    stop.store(true, Ordering::Relaxed);
    for burner in burners {
        burner.join().expect("burner thread");
    }
    result
}

fn holds_liquid(nodes: &[EffectGraphNode]) -> bool {
    nodes
        .iter()
        .any(|node| is_liquid_domain(&node.type_id) || node.group.as_ref().is_some_and(|group| holds_liquid(&group.nodes)))
}

/// BUG-qssh (thumbnail differs run to run): every bundled liquid preset's
/// thumbnail is the same bytes with every core busy as with the machine idle.
#[test]
fn liquid_thumbnail_ignores_contention() {
    let device = &harness::shared().device;
    let mut changed = Vec::new();
    let mut rendered = 0;
    for id in bundled_preset_type_ids(PresetKind::Generator) {
        let def = bundled_preset_def(&id).expect("bundled preset");
        if !holds_liquid(&def.nodes) {
            continue;
        }
        let render = || {
            render_preset_thumbnail(device, PresetKind::Generator, def, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT, false)
                .unwrap_or_else(|error| panic!("{id}: {error}"))
        };
        let start = Instant::now();
        let idle = render();
        let idle_time = start.elapsed();
        let busy = contended(render);
        let differing = idle.iter().zip(&busy).filter(|(a, b)| a != b).count();
        eprintln!(
            "{id}: {differing} of {} bytes differ; idle {idle_time:.1?}, busy {:.1?}",
            idle.len(),
            start.elapsed() - idle_time
        );
        if idle != busy {
            changed.push(id);
        }
        rendered += 1;
    }
    assert!(rendered > 0, "no bundled liquid preset");
    assert!(changed.is_empty(), "thumbnails changed under contention: {changed:?}");
}
