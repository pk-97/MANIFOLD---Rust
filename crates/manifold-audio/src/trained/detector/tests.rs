use super::*;
use crate::trained::golden::{self, Chunks};

/// Every decision from `push_traced` on a clip, fed in seeded chunks: (cand, avail, logits, p_net, p_final,
/// fired) plus the fires `push_with` returns on the same input.
type Trace = Vec<(u64, u64, Vec<f32>, f64, f64, bool)>;

fn trace(det: &mut TrainedDetector, audio: &[f32], seed: u64) -> (Trace, Vec<Fire>) {
    let mut out = Vec::new();
    let mut fires = Vec::new();
    let mut chunks = Chunks(seed);
    let mut at = 0;
    while at < audio.len() {
        let n = chunks.next().min(audio.len() - at);
        det.push_traced(&audio[at..at + n], |d, f| {
            assert_eq!(d.fire, f.is_some());
            out.push((d.cand_hop, d.avail_hop, d.net_logits.to_vec(), d.p_net, d.p_final, d.fire));
            fires.extend(f);
        });
        at += n;
    }
    (out, fires)
}

#[test]
fn traced_decisions_run_in_emission_order_and_carry_every_layer() {
    let m = golden::kick_v2();
    let g = golden::load("kick", &golden::clips("kick")[0]);
    let audio = golden::audio_f32(&g);
    let mut det = TrainedDetector::from_container(&m, 48_000).unwrap();
    assert_eq!((det.kind(), det.train_id()), (EventKind::Kick, m.text("train_id").unwrap()));
    let (decisions, fires) = trace(&mut det, &audio, 11);
    assert_eq!(decisions.len() as u64, det.decided());
    assert!(decisions.windows(2).all(|w| w[1].1 >= w[0].1), "decisions out of emission order");
    assert!(decisions.iter().all(|d| d.2.len() == 1 && d.3.is_finite() && d.4.is_finite()));
    let traced: Vec<(u64, u64)> = decisions.iter().filter(|d| d.5).map(|d| (d.0, d.1)).collect();
    let mut again = TrainedDetector::from_container(&m, 48_000).unwrap();
    let mut direct = Vec::new();
    again.push_with(&audio, |f| direct.push(f));
    assert_eq!(traced, fires.iter().map(|f| (f.cand_hop, f.avail_hop)).collect::<Vec<_>>());
    assert_eq!(fires, direct, "fires depend on chunking");
}

/// Two identical nets average to the one net's probability (D17 through the logit round trip), so the
/// detector's decisions match the single-net model's.
#[test]
fn several_nets_share_one_window() {
    let one = golden::kick_v2();
    let mut two = one.clone();
    two.insert("net.count", crate::trained::container::Array::i64s(&[], vec![2]));
    for p in crate::trained::spec::NET_PARAMS {
        let a = two.get(&format!("net.0.{p}")).unwrap().clone();
        two.insert(&format!("net.1.{p}"), a);
    }
    let g = golden::load("kick", &golden::clips("kick")[0]);
    let audio = golden::audio_f32(&g);
    let (a, _) = trace(&mut TrainedDetector::from_container(&one, 48_000).unwrap(), &audio, 5);
    let (b, _) = trace(&mut TrainedDetector::from_container(&two, 48_000).unwrap(), &audio, 5);
    assert_eq!(a.len(), b.len());
    let mut worst = 0.0f64;
    for (x, y) in a.iter().zip(&b) {
        assert_eq!((x.0, x.1), (y.0, y.1));
        assert_eq!(y.2, [x.2[0], x.2[0]], "both nets see one window");
        worst = worst.max((x.3 - y.3).abs());
    }
    println!("{} decisions, p_net one net vs two identical nets max |diff| {worst:.2e}", a.len());
    assert!(worst < 1e-9);
}

#[test]
fn refuses_a_zero_input_rate() {
    assert_eq!(TrainedDetector::from_container(&golden::kick_v2(), 0).err(), Some(DetectorError::BadRate(0)));
}

#[test]
fn refuses_a_v1_file() {
    let err = TrainedDetector::from_container(&golden::asset("kick"), 48_000).err();
    assert_eq!(err, Some(DetectorError::Spec(SpecError::Contract(None))));
}

#[test]
fn decision_lag_covers_the_net_look_ahead() {
    let kick = TrainedDetector::from_container(&golden::kick_v2(), 48_000).unwrap();
    let snare = TrainedDetector::from_container(&golden::snare_v2(), 48_000).unwrap();
    // Both nets look 40 ms past the emission, plus at most one 2 ms frame.
    for (name, d) in [("kick", &kick), ("snare", &snare)] {
        let lag = d.decision_lag_s();
        println!("{name}: decides up to {:.1} ms after emission", lag * 1e3);
        assert!((0.040..=0.0425).contains(&lag), "{name}: lag {lag}");
    }
}
