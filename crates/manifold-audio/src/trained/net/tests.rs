//! Net cost on the shipped kick. Net parity is in `trained::tests` (every golden set).

use super::*;
use crate::trained::frames::FrameBank;
use crate::trained::golden;
use crate::trained::spec::Spec;

/// Per-candidate cost; only a release run (`--release --no-capture`) says anything about the show.
#[test]
fn candidate_cost() {
    let model = golden::model("kick");
    let spec = Spec::parse(&model).unwrap();
    let mut net = Net::new(&model, 1).unwrap();
    let mut frames = FrameBank::new(&model, &spec.net.as_ref().unwrap().channels).unwrap();
    let audio: Vec<f64> = (0..48_000 * 2)
        .map(|i| ((i as f64) * 0.013).sin() * 0.3)
        .collect();
    let t0 = std::time::Instant::now();
    net.push(&audio);
    frames.push(&audio, Some(net.spectrum()));
    let push = t0.elapsed();
    let reps = 20;
    let t0 = std::time::Instant::now();
    let mut acc = 0.0;
    for r in 0..reps {
        let mut z = [0.0f32];
        net.logits(200 + r % 50, 202 + r % 50, &frames, &mut z);
        acc += average(&z, 1e-6);
    }
    let per = t0.elapsed() / reps as u32;
    assert!(acc.is_finite());
    eprintln!(
        "kick net: {:.1} us per candidate; push {:.2} ms per second of audio",
        per.as_secs_f64() * 1e6,
        push.as_secs_f64() * 1e3 / 2.0
    );
}
