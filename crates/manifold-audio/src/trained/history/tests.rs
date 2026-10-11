use super::*;
use crate::trained::detector::{FireRule, combine};
use crate::trained::golden;
use crate::trained::spec::{Combine, Measure};
use crate::trained::trees::tests::{container, f64e, i32e, i64e};

/// Stage forest: one tree splitting lp at 1.0 into -2 / +3.
fn model() -> Container {
    container(&[
        f64e("stage.value", &[0.0, -2.0, 3.0], false),
        f64e("stage.threshold", &[1.0, 0.0, 0.0], false),
        i32e("stage.feature", &[0, 0, 0]),
        i32e("stage.left", &[1, 0, 0]),
        i32e("stage.right", &[2, 0, 0]),
        i32e("stage.missing_left", &[0, 0, 0]),
        i32e("stage.is_leaf", &[0, 1, 1]),
        i64e("stage.offsets", &[0, 3], false),
        f64e("stage.baseline", &[0.0], true),
        i64e("stage.n_features", &[5], true),
    ])
}

/// The shipped kick's stage: one 8 s window on hstack(prof32, low16), level f15[0], no gate.
fn kick_stage() -> StageSpec {
    StageSpec {
        windows_s: vec![8.0],
        shape: vec![Measure::Prof32, Measure::Low16],
        level: Some((Measure::F15, 0)),
        gate: 0.0,
        cuts: None,
        power: 8.0,
        lp_clip: 1e-6,
        q_clip: 1e-6,
    }
}

/// Expected values from `self_features`, the blend and `fires` on the same stream (scratch script
/// stage_vals.py, scipy 1.17.1 / numpy 2.4.2). The window drops the first two candidates at the fifth.
#[test]
fn matches_the_python_stage_on_a_hand_made_stream() {
    let tree = [0.9, 0.2, 0.95, 0.6, 0.99, 0.97, 0.4];
    let net = [0.8, 0.1, 0.9, 0.7, 0.999, 0.5, 0.45];
    let avail = [10u64, 12, 400, 1000, 1700, 1704, 1712];
    let level = [1.0, -2.0, 3.0, 0.5, 2.5, 4.0, -1.0];
    let blend = [
        0.8571428571428572,
        0.14285714285714285,
        0.928960606878694,
        0.6516685226452116,
        0.9968302801438442,
        0.8504391264975321,
        0.42480768092719207,
    ];
    let want_x = [
        [1.7917594692280556, 0.0, 0.0, 0.0, 0.0],
        [-1.791759469228055, 0.4992034811813095, -3.5835189384561104, -3.0, 0.2556937210025549],
        [2.5708317782513297, 0.43419068203529493, 0.779074442556656, 2.0000017861214787, 0.25569385533139927],
        [0.6263814842476834, 0.5654751113254876, -1.676127593927203, -1.8111750943225196, 0.6129969929811535],
        [5.750937314391587, 0.5061644557133708, 3.2878219310204937, -0.3615079089287918, 0.4619228524941818],
        [1.738049344917636, 0.5591452721733352, -2.7771003663117853, 1.3641206848569767, 0.940805627286397],
        [-0.30306790178515775, 0.6631057822110182, -4.404270946171403, -3.8392112571636527, 1.0422759115106217],
    ];
    let mut s = History::new(&model(), &kick_stage(), 256, 48_000).unwrap();
    let mut rule = FireRule::new(0.5, 0.06);
    let mut fired = vec![];
    for k in 0..7 {
        let mut shape = [0.0; 48];
        for (j, v) in shape.iter_mut().enumerate() {
            *v = (0.3 * (k as f64 + 1.0) * (j as f64 + 1.0)).sin() + 0.1 * k as f64;
        }
        let blend_p = combine(Combine::MeanLogit, net[k], tree[k], 1e-6);
        assert!((blend_p - blend[k]).abs() < 1e-15, "blend {k}");
        let (x, p_stage, p_final) = s.decide(blend_p, &shape, level[k], avail[k] + 1);
        for (c, (a, b)) in x.iter().zip(&want_x[k]).enumerate() {
            assert!((a - b).abs() < 1e-12, "x[{k}][{c}] {a} vs {b}");
        }
        assert_eq!(p_stage, expit(if x[0] <= 1.0 { -2.0 } else { 3.0 }));
        assert_eq!(p_final, p_stage, "gate 0: the stage decides");
        if rule.decide(p_final, avail[k]) {
            fired.push(k);
        }
    }
    // Candidate 5 passes the cutoff 4 hops (21 ms) after the fire at 4: refractory.
    assert_eq!(fired, [0, 2, 4]);
    assert_eq!(s.dropped(), 0);
}

#[test]
fn a_full_history_drops_its_oldest() {
    let mut s = History::new(&model(), &kick_stage(), 256, 48_000).unwrap();
    let cap = s.emit_s.len();
    let shape = [1.0; 48];
    for k in 0..cap as u64 + 3 {
        s.decide(0.9, &shape, 0.0, 1);
        assert_eq!(s.len, cap.min(k as usize + 1));
    }
    assert_eq!(s.dropped(), 3);
}

/// One unit fixture through D17 averaging, the mean-logit combine, the history stage, the gate and the fire
/// rule. Fixtures `tests/fixtures/detectors/unit/history_{gate,plain}.mkick`, written by the scratch script
/// `make_unit.py` (kept outside the repo; P2 lane, 2026-10-11) from the lab's own functions: net
/// probabilities `torch.sigmoid` of f32 logits widened, D17 as `expit(np.mean([logit(clip(p_k))]))`,
/// `models.combine`-style mean logit, `lab/stage.py` `inputs` and `gated`, a sklearn
/// HistGradientBoostingClassifier as the stage forest (`lab/export/trees.py forest`), and
/// `detector_eval.fires`; numpy 2.4.2, scipy 1.17.1, sklearn 1.8.0, torch 2.14.1.
fn unit_history(file: &str, shape: Vec<Measure>, level: Option<(Measure, usize)>) {
    let g = golden::unit(file);
    let k = g.shape("avail_hop").unwrap()[0];
    let nets = g.shape("net.logit").unwrap()[1];
    let w = g.shape("windows_s").unwrap()[0];
    let gate = g.f64s("gate", &[]).unwrap()[0];
    let st = StageSpec {
        windows_s: g.f64s("windows_s", &[w]).unwrap().to_vec(),
        shape,
        level,
        gate,
        cuts: (gate > 0.0).then(|| {
            let c = g.f64s("cuts", &[2]).unwrap();
            (c[0], c[1])
        }),
        power: 8.0,
        lp_clip: 1e-6,
        q_clip: 1e-6,
    };
    assert_eq!(g.i64s("has_level", &[]).unwrap()[0] == 1, st.level.is_some());
    let width = st.shape_width();
    let n_in = st.inputs();
    let avail = g.i64s("avail_hop", &[k]).unwrap();
    let emit_s = g.f64s("emit_s", &[k]).unwrap();
    let shape_rows = g.f64s("shape", &[k, width]).unwrap();
    let level_v = g.f64s("level", &[k]).unwrap();
    let z = g.f32s("net.logit", &[k, nets]).unwrap();
    let p_k = g.f64s("p.k", &[k, nets]).unwrap();
    let (p_net, p_trees, p_comb) =
        (g.f64s("p.net", &[k]).unwrap(), g.f64s("p.trees", &[k]).unwrap(), g.f64s("p.combined", &[k]).unwrap());
    let (want_x, want_q, want_final) =
        (g.f64s("stage.x", &[k, n_in]).unwrap(), g.f64s("p.stage", &[k]).unwrap(), g.f64s("p.final", &[k]).unwrap());
    let nf = g.shape("fires").unwrap()[0];
    let want_fires = g.i64s("fires", &[nf]).unwrap();

    let mut h = History::new(&g, &st, 256, 48_000).unwrap();
    let mut rule = FireRule::new(g.f64s("cutoff", &[]).unwrap()[0], g.f64s("refractory_s", &[]).unwrap()[0]);
    let mut err = [0.0f64; 6];
    let mut fires = vec![];
    for i in 0..k {
        let zi = &z[i * nets..][..nets];
        for (j, &zj) in zi.iter().enumerate() {
            err[0] = err[0].max((crate::trained::net::probability(zj) - p_k[i * nets + j]).abs());
        }
        // D17 and the combine on Rust's own net probabilities: within the f32 sigmoid's last bit.
        err[1] = err[1].max((crate::trained::net::average(zi, 1e-6) - p_net[i]).abs());
        err[2] = err[2].max((combine(Combine::MeanLogit, p_net[i], p_trees[i], 1e-6) - p_comb[i]).abs());
        let a = avail[i] as u64;
        assert_eq!(((a + 1) * 256) as f64 / 48_000.0, emit_s[i], "emit_s {i}");
        // The history fed the golden p.combined: part parity.
        let (x, q, p_final) = h.decide(p_comb[i], &shape_rows[i * width..][..width], level_v[i], a + 1);
        for (c, v) in x.iter().enumerate() {
            err[3] = err[3].max((v - want_x[i * n_in + c]).abs());
        }
        err[4] = err[4].max((q - want_q[i]).abs());
        err[5] = err[5].max((p_final - want_final[i]).abs());
        if rule.decide(p_final, a) {
            fires.push(i as i64);
        }
    }
    println!(
        "{file}: {k} candidates, {nets} nets, {w} windows, gate {gate}: p_k {:.1e} p_net {:.1e} p_combined {:.1e} stage_x {:.1e} p_stage {:.1e} p_final {:.1e}; fires {}/{}",
        err[0],
        err[1],
        err[2],
        err[3],
        err[4],
        err[5],
        fires.len(),
        want_fires.len()
    );
    assert!(err[0] <= 1e-6 && err[1] <= 1e-6, "{file}: net probabilities {err:?}");
    assert!(err[2] <= 1e-12, "{file}: combine {err:?}");
    assert!(err[3] <= 1e-12 && err[4] <= 1e-12 && err[5] <= 1e-12, "{file}: history {err:?}");
    assert_eq!(fires, want_fires, "{file}: fires");
    assert_eq!(h.dropped(), 0);
}

#[test]
fn matches_the_lab_with_gate_two_windows_and_level() {
    unit_history("history_gate.mkick", vec![Measure::Prof32, Measure::Low16], Some((Measure::F15, 0)));
}

#[test]
fn matches_the_lab_without_gate_or_level() {
    unit_history("history_plain.mkick", vec![Measure::Prof32, Measure::Low16], None);
}
