use super::*;
use crate::trained::golden;

fn max_diff(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len());
    a.iter()
        .zip(b)
        .map(|(a, b)| (a - b).abs())
        .fold(0.0, f32::max)
}

/// The excerpt fixture: 6 s of a 128 BPM mix at 22050 Hz with torchaudio's spectrogram and PyTorch's logits.
fn excerpt() -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
    let g = golden::load("beats", "excerpt.mkick");
    let n = g.shape("signal").unwrap()[0];
    let frames = g.shape("mel").unwrap()[0];
    let get = |name: &str, shape: &[usize]| g.f32s(name, shape).unwrap().to_vec();
    (
        get("signal", &[n]),
        get("mel", &[frames, BANDS]),
        get("beat", &[frames]),
        get("downbeat", &[frames]),
    )
}

#[test]
fn spectrogram_matches_torchaudio() {
    let (signal, mel, _, _) = excerpt();
    let ours = mel::log_mel(&signal);
    let diff = max_diff(&ours, &mel);
    assert!(diff < 1e-3, "log-mel differs by {diff}");
}

#[test]
fn network_matches_pytorch() {
    let (_, mel, beat, downbeat) = excerpt();
    let (b, d) = model::Model::load().unwrap().logits(&mel);
    let (db, dd) = (max_diff(&b, &beat), max_diff(&d, &downbeat));
    assert!(
        db < 1e-2 && dd < 1e-2,
        "beat logits differ by {db}, downbeat by {dd}"
    );
}

#[test]
fn chunks_follow_split_piece() {
    assert_eq!(chunk_starts(700), vec![-6]);
    assert_eq!(chunk_starts(1488), vec![-6]);
    assert_eq!(chunk_starts(1489), vec![-6, -5]);
    assert_eq!(chunk_starts(3000), vec![-6, 1482, 1506]);
}

/// Logits at 50 fps with a bump at each beat time of `beats`, highest at `downbeats`.
fn activation(len_s: f64, beats: &[f64], downbeats: &[f64]) -> (Vec<f32>, Vec<f32>) {
    let frames = (len_s * FPS) as usize;
    let bump = |times: &[f64], f: usize| {
        times
            .iter()
            .map(|t| 6.0 - 8.0 * ((f as f64 - t * FPS) / 1.2).powi(2))
            .fold(-6.0f64, f64::max) as f32
    };
    let beat = (0..frames).map(|f| bump(beats, f)).collect();
    let downbeat = (0..frames).map(|f| bump(downbeats, f)).collect();
    (beat, downbeat)
}

#[test]
fn fit_finds_tempo_and_bar_one_through_gaps_and_strays() {
    let period = 60.0 / 127.4;
    let origin = 0.43;
    let grid: Vec<f64> = (0..160).map(|k| origin + period * k as f64).collect();
    // Every 9th beat missing, a ±8 ms wobble, and two off-beat strays.
    let mut beats: Vec<f64> = grid
        .iter()
        .enumerate()
        .filter(|(k, _)| k % 9 != 4)
        .map(|(k, t)| t + 0.008 * ((k * 7 % 5) as f64 - 2.0) / 2.0)
        .collect();
    beats.extend([grid[20] + period / 2.0, grid[71] + period / 2.0]);
    beats.sort_by(f64::total_cmp);
    // Bar 1 is the third beat of the grid's count.
    let downbeats: Vec<f64> = grid.iter().skip(2).step_by(4).copied().collect();
    let (beat, downbeat) = activation(grid[159] + 1.0, &beats, &downbeats);

    let (g, _) = fit(&beat, &downbeat, 4, MIN_BEATS).unwrap();
    assert!((g.bpm() - 127.4).abs() < 0.02, "bpm {}", g.bpm());
    let bar = 4.0 * period;
    let off = (g.downbeat_s - grid[2]).rem_euclid(bar);
    assert!(off.min(bar - off) < 0.01, "bar 1 off by {off} s");
    assert!((g.downbeat_at_or_after(1.5) - grid[6]).abs() < 0.01);
}

#[test]
fn fit_refuses_a_tempo_change() {
    let first: Vec<f64> = (0..80).map(|k| 0.5 + 0.5 * k as f64).collect();
    let second: Vec<f64> = (1..80).map(|k| 40.0 + 0.42 * k as f64).collect();
    let beats: Vec<f64> = first.iter().chain(&second).copied().collect();
    let (beat, downbeat) = activation(75.0, &beats, &[]);
    assert!(matches!(
        fit(&beat, &downbeat, 4, MIN_BEATS),
        Err(NoGrid::Unsteady { .. })
    ));
}

#[test]
fn bar_one_moves_only_after_enough_votes() {
    use super::follow::BarVote;
    let p = 0.5;
    let mut bar = BarVote::default();
    assert_eq!(bar.settle(1.0, p, 4, 4), 1.0);
    // Next update agrees (a bar later): follows it.
    assert_eq!(bar.settle(3.0, p, 4, 4), 3.0);
    // Three updates say bar 1 is a beat later: held where it was.
    for vote in [5.5, 7.5, 9.5] {
        let kept = bar.settle(vote, p, 4, 4);
        assert!(
            ((kept - vote) / p).round() as i64 % 4 != 0,
            "held at {kept}"
        );
    }
    // The fourth in a row moves it.
    assert_eq!(bar.settle(11.5, p, 4, 4), 11.5);
}

#[test]
fn tempo_holds_until_enough_hearings_agree() {
    use super::follow::TempoHold;
    let grid = |period_s: f64, downbeat_s: f64| Grid {
        period_s,
        downbeat_s,
        beats_per_bar: 4,
    };
    let mut hold = TempoHold::default();
    assert_eq!(hold.settle(grid(0.5, 1.0), 3), grid(0.5, 1.0));
    // Double tempo with its downbeat on an off-beat: folded back, onto the held beats.
    assert_eq!(hold.settle(grid(0.25, 3.25), 3), grid(0.5, 3.5));
    // Half tempo: every heard beat is a held beat.
    assert_eq!(hold.settle(grid(1.0, 5.0), 3), grid(0.5, 5.0));
    // Four times, its downbeat three quarter-beats past a held beat: folded onto the next held beat.
    assert_eq!(hold.settle(grid(0.125, 6.375), 3), grid(0.5, 6.5));
    // A small change is taken at once.
    assert_eq!(hold.settle(grid(0.49, 7.0), 3), grid(0.49, 7.0));
    // An unrelated tempo keeps the held grid.
    assert_eq!(hold.settle(grid(0.3, 8.0), 3), grid(0.49, 7.0));
    // Three hearings in a row of one new tempo win.
    assert_eq!(hold.settle(grid(0.245, 9.0), 3), grid(0.49, 9.0));
    assert_eq!(hold.settle(grid(0.245, 11.0), 3), grid(0.49, 11.0));
    assert_eq!(hold.settle(grid(0.245, 13.0), 3), grid(0.245, 13.0));
}

#[test]
fn a_peak_beside_masked_frames_stays_finite() {
    let mut logits = vec![-5.0f32; 40];
    logits[10] = f32::NEG_INFINITY;
    logits[11] = 3.0;
    logits[30] = 4.0;
    logits[31] = f32::NEG_INFINITY;
    let times = pick(&logits);
    assert_eq!(times, vec![11.0 / FPS, 30.0 / FPS]);
}
