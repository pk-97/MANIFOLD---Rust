use super::*;
use crate::trained::golden;

/// The reference's past-4 s max reads ahead for 1 ms grid indices under this (scipy reflect
/// boundary); the causal port is compared from here on only.
const TAIL_CAUSAL_FROM_MS: i64 = 1999 + 5;

/// Streams `audio` in seeded chunks and writes `measures` (hstacked) for every candidate once it is ready.
/// Candidates within a hop of the clip end are left NaN: the reference truncates windows there.
fn run(bank: &mut MeasureBank, audio: &[f64], cands: &[Candidate], measures: &[Measure], seed: u64) -> Vec<Vec<f64>> {
    let width: usize = measures.iter().map(|m| m.width()).sum();
    let ready: Vec<u64> = cands.iter().map(|c| bank.ready_sample(c)).collect();
    let mut got = vec![vec![f64::NAN; width]; cands.len()];
    let mut order: Vec<usize> = (0..cands.len()).filter(|&i| ready[i] + 256 <= audio.len() as u64).collect();
    order.sort_by_key(|&i| ready[i]);
    let (mut seed, mut at, mut next) = (seed, 0usize, 0usize);
    while at < audio.len() {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let end = (at + 256 + ((seed >> 33) % 769) as usize).min(audio.len());
        bank.push(&audio[at..end]);
        at = end;
        while next < order.len() && ready[order[next]] <= bank.samples() {
            let i = order[next];
            let mut col = 0;
            for &m in measures {
                bank.write(m, &cands[i], &mut got[i][col..col + m.width()]);
                col += m.width();
            }
            next += 1;
        }
    }
    got
}

/// Max abs error per measure against `want[m]` (n, width); tail3's pre_rel_db is skipped for onsets the
/// reference computes with its reflected (non-causal) start. Returns (measure, max err, values over 1e-6).
fn compare(got: &[Vec<f64>], want: &[(Measure, &[f64])], onset_s: &[f64]) -> Vec<(Measure, f64, usize)> {
    let mut col = 0;
    let mut out = Vec::new();
    for &(m, w) in want {
        let (mut max_err, mut bad) = (0.0f64, 0);
        for (i, row) in got.iter().enumerate() {
            if row[col].is_nan() {
                continue;
            }
            let onset_ms = (onset_s[i] / 0.001).round_ties_even() as i64;
            for j in 0..m.width() {
                if m == Measure::Tail3 && j == 1 && (60..TAIL_CAUSAL_FROM_MS).contains(&onset_ms) {
                    continue;
                }
                let err = (row[col + j] - w[i * m.width() + j]).abs();
                if err.is_nan() || err > 1e-6 {
                    bad += 1;
                }
                if !err.is_nan() {
                    max_err = max_err.max(err);
                }
            }
        }
        out.push((m, max_err, bad));
        col += m.width();
    }
    out
}

/// The kick v1 goldens' tail3, tmpl3, prof32 and low16 from audio, on the kick finder's candidates.
#[test]
fn v1_kick_measures_match_every_golden_clip() {
    let model = golden::kick_v2();
    let measures = [Measure::Tail3, Measure::Tmpl3, Measure::Prof32, Measure::Low16];
    for name in golden::clips("kick") {
        let g = golden::load("kick", &name);
        let audio = golden::audio_f64(&g);
        let k = g.shape("cand_hop").unwrap()[0];
        let (cand, avail) = (g.i64s("cand_hop", &[k]).unwrap(), g.i64s("avail_hop", &[k]).unwrap());
        let cands: Vec<Candidate> =
            (0..k).map(|i| Candidate { cand_hop: cand[i] as u64, avail_hop: avail[i] as u64, f15: [0.0; 15] }).collect();
        let mut bank = MeasureBank::new(&model, &measures).unwrap();
        let got = run(&mut bank, &audio, &cands, &measures, 0x9E37_79B9_7F4A_7C15 ^ name.len() as u64);
        let want: Vec<(Measure, &[f64])> = measures.iter().map(|&m| (m, g.f64s(m.name(), &[k, m.width()]).unwrap())).collect();
        for (m, err, bad) in compare(&got, &want, g.f64s("onset_s", &[k]).unwrap()) {
            println!("{name} {}: max abs err {err:.3e}, {bad} over 1e-6", m.name());
            assert_eq!(bad, 0, "{name} {}: {bad} values over 1e-6", m.name());
        }
    }
}

/// tail3, prof32, fall16 and low16 on hand-placed candidates (any finder) of a synthetic clip, against the
/// lab's measure builders. Fixture `tests/fixtures/detectors/unit/measures.mkick`, written by the scratch
/// script `make_unit.py` (kept outside the repo; P2 lane, 2026-10-11): `components.MEASURES[name](x, 48000,
/// components.Candidates(cand, avail, 256, 48000))` on `x = audio_i16 / 32768`; numpy 2.4.2, scipy 1.17.1.
#[test]
fn measures_match_the_lab_on_any_candidates() {
    let model = golden::kick_v2();
    let g = golden::unit("measures.mkick");
    let measures = [Measure::Tail3, Measure::Prof32, Measure::Fall16, Measure::Low16];
    let audio = golden::audio_f64(&g);
    let k = g.shape("cand_hop").unwrap()[0];
    let (cand, avail) = (g.i64s("cand_hop", &[k]).unwrap(), g.i64s("avail_hop", &[k]).unwrap());
    let cands: Vec<Candidate> =
        (0..k).map(|i| Candidate { cand_hop: cand[i] as u64, avail_hop: avail[i] as u64, f15: [0.0; 15] }).collect();
    let mut bank = MeasureBank::new(&model, &measures).unwrap();
    let got = run(&mut bank, &audio, &cands, &measures, 3);
    let compared = got.iter().filter(|r| !r[0].is_nan()).count();
    assert!(compared + 2 >= k, "only {compared} of {k} candidates compared");
    let names: Vec<String> = measures.iter().map(|m| format!("m.{}", m.name())).collect();
    let want: Vec<(Measure, &[f64])> =
        measures.iter().zip(&names).map(|(&m, n)| (m, g.f64s(n, &[k, m.width()]).unwrap())).collect();
    for (m, err, bad) in compare(&got, &want, g.f64s("onset_s", &[k]).unwrap()) {
        println!("unit {}: {compared} of {k} candidates, max abs err {err:.3e}, {bad} over 1e-6", m.name());
        assert_eq!(bad, 0, "{}: {bad} values over 1e-6", m.name());
    }
    // fall16 is not all zeros on this clip: the comparison means something.
    let fall = want[2].1;
    assert!(fall.iter().filter(|v| v.abs() > 1.0).count() > k, "fall16 fixture is degenerate");
}

#[test]
fn builds_only_the_streams_its_measures_read() {
    let model = golden::kick_v2();
    let bank = MeasureBank::new(&model, &[Measure::F15]).unwrap();
    assert!(bank.k.is_none() && bank.spec.is_none() && bank.tail.is_none() && bank.low.is_none());
    let bank = MeasureBank::new(&model, &[Measure::Fall16]).unwrap();
    assert!(bank.spec.is_some() && bank.templates.is_none() && bank.tail.is_none() && bank.low.is_none());
}
