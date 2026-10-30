//! Scores the live beat follower on recorded songs as if they were streaming in. The network hears each song once a
//! second for each window length, on all cores, and the hearings are cached in CACHE_DIR; every tracker tuning is
//! then scored on the same hearings in seconds. Truth is every beat and downbeat Beat This! picks looking at the
//! whole song. Between hearings, and while a hearing finds no lock, the beats come from the last lock, as the
//! transport would keep running.
//!
//!     cargo run --release -p manifold-audio --example follow_sim -- CACHE_DIR SONG.wav [MORE.wav ...]
//!
//! Prints the best tunings over all songs, writes every tuning's score to CACHE_DIR/sweep.txt, and writes a timeline
//! per song, one line a second, for `Tuning::LIVE`.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use manifold_audio::beats::{Heard, Listener, Lock, Tracker, Tuning, song_beats};

/// Samples per push, as an audio callback would deliver them.
const BLOCK: usize = 512;
/// The window lengths heard.
const WINDOWS_S: [f64; 4] = [6.0, 8.0, 10.0, 12.0];
/// A predicted beat this close to a truth beat is a hit.
const TOLERANCE_S: f64 = 0.07;
/// Hearings per parallel job; each job starts its own listener a window early.
const JOB: usize = 60;
const BEATS_PER_BAR: u32 = 4;

struct Song {
    name: String,
    beats: Vec<f64>,
    downbeats: Vec<f64>,
    /// Per entry of WINDOWS_S: hearings, one a second.
    hearings: Vec<Vec<(f64, Heard)>>,
}

#[derive(Default, Clone)]
struct Score {
    hits: usize,
    predicted: usize,
    truth: usize,
    found: usize,
    bar_right: usize,
    bar_checked: usize,
    hearings: usize,
    locked: usize,
    octave_flips: usize,
    tempo_jumps: usize,
    bar_flips: usize,
    /// BPM change between neighbouring locks at the same tempo.
    wobble: Vec<f64>,
}

impl Score {
    fn add(&mut self, o: &Score) {
        self.hits += o.hits;
        self.predicted += o.predicted;
        self.truth += o.truth;
        self.found += o.found;
        self.bar_right += o.bar_right;
        self.bar_checked += o.bar_checked;
        self.hearings += o.hearings;
        self.locked += o.locked;
        self.octave_flips += o.octave_flips;
        self.tempo_jumps += o.tempo_jumps;
        self.bar_flips += o.bar_flips;
        self.wobble.extend(&o.wobble);
    }

    fn f1(&self) -> f64 {
        let p = self.hits as f64 / self.predicted.max(1) as f64;
        let r = self.found as f64 / self.truth.max(1) as f64;
        2.0 * p * r / (p + r).max(1e-9)
    }

    fn line(&self) -> String {
        let pct = |a: usize, b: usize| a as f64 / b.max(1) as f64 * 100.0;
        let mut w = self.wobble.clone();
        w.sort_by(f64::total_cmp);
        format!(
            "F1 {:5.1}  hits {:5.1}%  found {:5.1}%  bar {:5.1}%  locked {:5.1}%  octave flips {:3}  jumps {:3}  bar flips {:3}  wobble {:.2} BPM",
            self.f1() * 100.0,
            pct(self.hits, self.predicted),
            pct(self.found, self.truth),
            pct(self.bar_right, self.bar_checked),
            pct(self.locked, self.hearings),
            self.octave_flips,
            self.tempo_jumps,
            self.bar_flips,
            w.get(w.len() / 2).copied().unwrap_or(0.0)
        )
    }
}

fn near(sorted: &[f64], t: f64) -> bool {
    let i = sorted.partition_point(|b| *b < t);
    [i.checked_sub(1), Some(i)]
        .into_iter()
        .flatten()
        .filter_map(|j| sorted.get(j))
        .any(|b| (b - t).abs() <= TOLERANCE_S)
}

/// Tempo from the median gap of the truth beats within 4 s of `t`.
fn truth_bpm(beats: &[f64], t: f64) -> f64 {
    let from = beats.partition_point(|b| *b < t - 4.0);
    let to = beats.partition_point(|b| *b <= t + 4.0);
    let mut gaps: Vec<f64> = beats[from..to].windows(2).map(|w| w[1] - w[0]).collect();
    if gaps.is_empty() {
        return 0.0;
    }
    gaps.sort_by(f64::total_cmp);
    60.0 / gaps[gaps.len() / 2]
}

/// Runs a tracker with `tuning` over the song's cached hearings, every `hear_every_s` seconds, scoring the beats each lock
/// predicts until the next hearing. With `timeline`, writes a line per hearing.
fn score(song: &Song, tuning: Tuning, mut timeline: Option<&mut String>) -> Score {
    let every = tuning.hear_every_s as usize;
    let w = WINDOWS_S
        .iter()
        .position(|w| *w == tuning.window_s)
        .expect("a cached window");
    let hearings = &song.hearings[w];
    let mut tracker = Tracker::new(tuning, BEATS_PER_BAR);
    let mut s = Score::default();
    let mut last: Option<Lock> = None;
    for (now, heard) in hearings.iter().skip(every - 1).step_by(every) {
        let (now, until) = (*now, *now + every as f64);
        s.hearings += 1;
        let result = tracker.lock(heard);
        if let Ok(lock) = &result {
            s.locked += 1;
            if let Some(prev) = last {
                let ratio = lock.grid.period_s / prev.grid.period_s;
                if [2.0, 0.5].iter().any(|f| (ratio / f - 1.0).abs() < 0.04) {
                    s.octave_flips += 1;
                } else if (ratio - 1.0).abs() > 0.03 {
                    s.tempo_jumps += 1;
                } else {
                    s.wobble.push((lock.grid.bpm() - prev.grid.bpm()).abs());
                    let beats = ((lock.grid.downbeat_s - prev.grid.downbeat_s)
                        / lock.grid.period_s)
                        .round();
                    if (beats as i64).rem_euclid(i64::from(BEATS_PER_BAR)) != 0 {
                        s.bar_flips += 1;
                    }
                }
            }
            last = Some(*lock);
        }
        let from = song.beats.partition_point(|b| *b <= now);
        let to = song.beats.partition_point(|b| *b <= until);
        let truth = &song.beats[from..to];
        let mut predicted = Vec::new();
        if let Some(lock) = last {
            let g = lock.grid;
            let mut k = ((now - g.downbeat_s) / g.period_s).floor() as i64 + 1;
            loop {
                let t = g.downbeat_s + k as f64 * g.period_s;
                if t > until {
                    break;
                }
                if t > now {
                    predicted.push((t, k.rem_euclid(i64::from(BEATS_PER_BAR)) == 0));
                }
                k += 1;
            }
        }
        let hits = predicted
            .iter()
            .filter(|(t, _)| near(&song.beats, *t))
            .count();
        let found = truth
            .iter()
            .filter(|b| predicted.iter().any(|(t, _)| (t - *b).abs() <= TOLERANCE_S))
            .count();
        let mut bar_right = 0;
        let mut bar_checked = 0;
        for (t, _) in predicted
            .iter()
            .filter(|(t, down)| *down && near(&song.beats, *t))
        {
            bar_checked += 1;
            bar_right += usize::from(near(&song.downbeats, *t));
        }
        s.hits += hits;
        s.predicted += predicted.len();
        s.truth += truth.len();
        s.found += found;
        s.bar_right += bar_right;
        s.bar_checked += bar_checked;
        if let Some(out) = timeline.as_deref_mut() {
            let clock = format!("{:3}:{:02}", now as u64 / 60, now as u64 % 60);
            let state = match (&result, last) {
                (Ok(lock), _) => format!(
                    "{:7.2} BPM  grid {:3.0}%",
                    lock.grid.bpm(),
                    lock.on_grid * 100.0
                ),
                (Err(why), Some(l)) => format!("{:7.2} kept  ({why})", l.grid.bpm()),
                (Err(why), None) => format!("   none      ({why})"),
            };
            let bar = match bar_checked {
                0 => "",
                _ if bar_right == bar_checked => "  bar 1 right",
                _ => "  bar 1 WRONG",
            };
            out.push_str(&format!(
                "{clock}  {state:<46} truth {:6.1}  beats {hits}/{} hit, {found}/{} found{bar}\n",
                truth_bpm(&song.beats, now),
                predicted.len(),
                truth.len()
            ));
        }
    }
    s
}

/// Hears `mono` once a second with a listener over `window_s`, in parallel jobs that each start a window early.
fn hear_all(mono: &[f32], rate: u32, window_s: f64, threads: usize) -> Vec<(f64, Heard)> {
    let step = rate as usize;
    let count = mono.len() / step;
    let jobs: Vec<std::ops::Range<usize>> = (1..=count)
        .step_by(JOB)
        .map(|k| k..(k + JOB).min(count + 1))
        .collect();
    let next = AtomicUsize::new(0);
    let mut out: Vec<(f64, Heard)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (jobs, next) = (&jobs, &next);
                scope.spawn(move || {
                    let mut out = Vec::new();
                    while let Some(ks) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let lead = ((window_s + 1.0) * f64::from(rate)) as usize;
                        let s0 = (ks.start * step).saturating_sub(lead);
                        let mut listener = Listener::new(rate, window_s);
                        let mut fed = s0;
                        for k in ks.clone() {
                            while fed < k * step {
                                let end = (fed + BLOCK).min(k * step);
                                listener.push(&mono[fed..end]);
                                fed = end;
                            }
                            let mut heard = listener.hear();
                            heard.start_s += s0 as f64 / f64::from(rate);
                            out.push((fed as f64 / f64::from(rate), heard));
                        }
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a hearing job panicked"))
            .collect()
    });
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

/// Seconds of CPU the whole process has used, every thread counted.
fn process_cpu_s() -> f64 {
    #[repr(C)]
    struct Timespec {
        sec: i64,
        nsec: i64,
    }
    unsafe extern "C" {
        fn clock_gettime(clock: u32, out: *mut Timespec) -> i32;
    }
    #[cfg(target_os = "macos")]
    const PROCESS_CPU: u32 = 12;
    #[cfg(not(target_os = "macos"))]
    const PROCESS_CPU: u32 = 2;
    let mut t = Timespec { sec: 0, nsec: 0 };
    // SAFETY: `t` is a valid timespec for the call to fill.
    unsafe { clock_gettime(PROCESS_CPU, &mut t) };
    t.sec as f64 + t.nsec as f64 * 1e-9
}

/// Seconds of CPU one hearing takes, every thread the maths uses counted, for each entry of WINDOWS_S: the median
/// of five over the middle of `mono`, with nothing else running.
fn cost(mono: &[f32], rate: u32) -> Vec<f64> {
    WINDOWS_S
        .iter()
        .map(|&window_s| {
            let mut listener = Listener::new(rate, window_s);
            let from = mono.len() / 2;
            listener.push(
                &mono[from..(from + ((window_s + 1.0) * f64::from(rate)) as usize).min(mono.len())],
            );
            let mut times: Vec<f64> = (0..5)
                .map(|_| {
                    let started = process_cpu_s();
                    std::hint::black_box(listener.hear());
                    process_cpu_s() - started
                })
                .collect();
            times.sort_by(f64::total_cmp);
            times[2]
        })
        .collect()
}

fn put_f64s(buf: &mut Vec<u8>, xs: &[f64]) {
    buf.extend((xs.len() as u64).to_le_bytes());
    xs.iter().for_each(|x| buf.extend(x.to_le_bytes()));
}

fn put_f32s(buf: &mut Vec<u8>, xs: &[f32]) {
    buf.extend((xs.len() as u64).to_le_bytes());
    xs.iter().for_each(|x| buf.extend(x.to_le_bytes()));
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let (head, rest) = self.0.split_at(N);
        self.0 = rest;
        head.try_into().expect("N bytes")
    }
    fn len(&mut self) -> usize {
        u64::from_le_bytes(self.bytes()) as usize
    }
    fn f64s(&mut self) -> Vec<f64> {
        (0..self.len())
            .map(|_| f64::from_le_bytes(self.bytes()))
            .collect()
    }
    fn f32s(&mut self) -> Vec<f32> {
        (0..self.len())
            .map(|_| f32::from_le_bytes(self.bytes()))
            .collect()
    }
}

fn decode(path: &str) -> Option<(Vec<f32>, u32)> {
    match manifold_playback::audio_decoder::decode_audio_to_pcm(path) {
        Ok(a) => Some((a.mono(), a.sample_rate)),
        Err(e) => {
            println!("{path}: {e}");
            None
        }
    }
}

/// The song's truth and hearings, each from the cache when there.
fn load(path: &str, cache: &std::path::Path, threads: usize) -> Option<Song> {
    let name = path.rsplit('/').next().unwrap_or(path).to_owned();
    // Decoded only when something is missing from the cache.
    let mut audio: Option<(Vec<f32>, u32)> = None;
    let truth_file = cache.join(format!("{name}.truth"));
    let (beats, downbeats) = match std::fs::read(&truth_file) {
        Ok(bytes) => {
            let mut r = Reader(&bytes);
            (r.f64s(), r.f64s())
        }
        Err(_) => {
            if audio.is_none() {
                audio = Some(decode(path)?);
            }
            let (mono, rate) = audio.as_ref().expect("decoded above");
            let truth = song_beats(mono, *rate);
            let mut buf = Vec::new();
            put_f64s(&mut buf, &truth.0);
            put_f64s(&mut buf, &truth.1);
            std::fs::write(&truth_file, buf).expect("the cache folder is writable");
            truth
        }
    };
    let mut hearings = Vec::with_capacity(WINDOWS_S.len());
    for window_s in WINDOWS_S {
        let file = cache.join(format!("{name}.w{window_s}.hearings"));
        if let Ok(bytes) = std::fs::read(&file) {
            let mut r = Reader(&bytes);
            hearings.push(
                (0..r.len())
                    .map(|_| {
                        let now = f64::from_le_bytes(r.bytes());
                        let start_s = f64::from_le_bytes(r.bytes());
                        let (beat, downbeat) = (r.f32s(), r.f32s());
                        (
                            now,
                            Heard {
                                beat,
                                downbeat,
                                start_s,
                            },
                        )
                    })
                    .collect(),
            );
            continue;
        }
        if audio.is_none() {
            audio = Some(decode(path)?);
        }
        let (mono, rate) = audio.as_ref().expect("decoded above");
        let started = Instant::now();
        let list = hear_all(mono, *rate, window_s, threads);
        eprintln!(
            "{name}: {window_s} s window heard in {:.0} s",
            started.elapsed().as_secs_f64()
        );
        let mut buf = Vec::new();
        buf.extend((list.len() as u64).to_le_bytes());
        for (now, h) in &list {
            buf.extend(now.to_le_bytes());
            buf.extend(h.start_s.to_le_bytes());
            put_f32s(&mut buf, &h.beat);
            put_f32s(&mut buf, &h.downbeat);
        }
        std::fs::write(&file, buf).expect("the cache folder is writable");
        hearings.push(list);
    }
    Some(Song {
        name,
        beats,
        downbeats,
        hearings,
    })
}

fn main() {
    let mut args = std::env::args().skip(1);
    let cache = std::path::PathBuf::from(args.next().expect("CACHE_DIR SONG.wav ..."));
    std::fs::create_dir_all(&cache).expect("the cache folder can be made");
    let threads =
        std::thread::available_parallelism().map_or(1, |n| n.get().saturating_sub(4).max(1));
    let paths: Vec<String> = args.collect();
    let songs: Vec<Song> = paths
        .iter()
        .filter_map(|p| load(p, &cache, threads))
        .collect();
    let costs = paths
        .first()
        .and_then(|p| decode(p))
        .map(|(mono, rate)| cost(&mono, rate));
    if let Some(costs) = &costs {
        println!("CPU per hearing, every thread counted:");
        for (window_s, c) in WINDOWS_S.iter().zip(costs) {
            println!("  {window_s:2.0} s window  {:5.0} ms", c * 1000.0);
        }
    }
    let cost_of = |t: &Tuning| {
        let i = WINDOWS_S
            .iter()
            .position(|w| *w == t.window_s)
            .expect("a heard window");
        costs.as_ref().map_or(f64::NAN, |c| c[i] / t.hear_every_s)
    };

    let mut tunings = Vec::new();
    for window_s in WINDOWS_S {
        for fit_s in [4.0, 6.0, 8.0, 10.0] {
            if fit_s > window_s - 0.5 {
                continue;
            }
            for min_beats in [6, 8] {
                for bar_votes in [3, 5] {
                    for tempo_votes in [4, 6, 8] {
                        for hear_every_s in [1.0, 2.0] {
                            tunings.push(Tuning {
                                window_s,
                                fit_s,
                                min_beats,
                                bar_votes,
                                tempo_votes,
                                hear_every_s,
                            });
                        }
                    }
                }
            }
        }
    }
    let next = AtomicUsize::new(0);
    let mut results: Vec<(Tuning, Score)> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..threads)
            .map(|_| {
                let (tunings, next, songs) = (&tunings, &next, &songs);
                scope.spawn(move || {
                    let mut out = Vec::new();
                    while let Some(&t) = tunings.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let mut total = Score::default();
                        songs.iter().for_each(|s| total.add(&score(s, t, None)));
                        out.push((t, total));
                    }
                    out
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("a scoring job panicked"))
            .collect()
    });
    results.sort_by(|a, b| b.1.f1().total_cmp(&a.1.f1()));
    let label = |t: &Tuning| {
        format!(
            "window {:2.0} fit {:2} bar votes {} tempo votes {} every {} s  CPU {:4.1}%",
            t.window_s,
            t.fit_s,
            t.bar_votes,
            t.tempo_votes,
            t.hear_every_s,
            cost_of(t) * 100.0
        )
    };
    let all: String = results
        .iter()
        .map(|(t, s)| format!("{:<78} {}\n", label(t), s.line()))
        .collect();
    std::fs::write(cache.join("sweep.txt"), all).expect("the cache folder is writable");
    println!(
        "Best of {} tunings over {} songs (all in sweep.txt), CPU as a share of one core:",
        results.len(),
        songs.len()
    );
    for (t, s) in results.iter().take(20) {
        println!("  {:<78} {}", label(t), s.line());
    }
    println!("Best per window and rate:");
    for window_s in WINDOWS_S {
        for every in [1.0, 2.0] {
            if let Some((t, s)) = results
                .iter()
                .find(|r| (r.0.window_s, r.0.hear_every_s) == (window_s, every))
            {
                println!("  {:<78} {}", label(t), s.line());
            }
        }
    }

    println!("\nTuning::LIVE ({}) per song:", label(&Tuning::LIVE));
    for song in &songs {
        let mut timeline = String::new();
        let s = score(song, Tuning::LIVE, Some(&mut timeline));
        println!("  {:<70} {}", song.name, s.line());
        std::fs::write(cache.join(format!("{}.timeline.txt", song.name)), timeline)
            .expect("the cache folder is writable");
    }
}
