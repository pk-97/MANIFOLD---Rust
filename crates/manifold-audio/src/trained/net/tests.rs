//! Net cost on the shipped kick. Net parity is in `trained::tests` (every golden set).

use super::*;
use crate::trained::golden;

/// Per-candidate cost; only a release run (`--release --no-capture`) says anything about the show.
#[test]
fn candidate_cost() {
    let model = golden::model("kick");
    let mut net = Net::new(&model, 1).unwrap();
    let audio: Vec<f64> = (0..48_000 * 2).map(|i| ((i as f64) * 0.013).sin() * 0.3).collect();
    let t0 = std::time::Instant::now();
    net.push(&audio);
    let push = t0.elapsed();
    let reps = 20;
    let t0 = std::time::Instant::now();
    let mut acc = 0.0;
    for r in 0..reps {
        let mut z = [0.0f32];
        net.logits(200 + r % 50, 202 + r % 50, &NoFrames, &mut z);
        acc += average(&z, 1e-6);
    }
    let per = t0.elapsed() / reps as u32;
    assert!(acc.is_finite());
    eprintln!("kick net: {:.1} us per candidate; push {:.2} ms per second of audio", per.as_secs_f64() * 1e6, push.as_secs_f64() * 1e3 / 2.0);
}
