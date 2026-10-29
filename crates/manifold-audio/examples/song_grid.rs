//! Prints the beat grid Detect finds in each audio file: tempo, first downbeat and how long the beat tracker took.
//!
//!     cargo run --release -p manifold-audio --example song_grid -- SONG.wav [MORE.wav ...]

fn main() {
    for path in std::env::args().skip(1) {
        let audio = match manifold_playback::audio_decoder::decode_audio_to_pcm(&path) {
            Ok(a) => a,
            Err(e) => {
                println!("{path}: {e}");
                continue;
            }
        };
        let mono = audio.mono();
        let started = std::time::Instant::now();
        let grid = manifold_audio::beats::song_grid(&mono, audio.sample_rate, 4);
        let took = started.elapsed().as_secs_f64();
        let len = mono.len() as f64 / f64::from(audio.sample_rate);
        match grid {
            Ok(g) => println!(
                "{path}: {:.2} BPM, first downbeat {:.3} s ({len:.0} s of audio in {took:.2} s)",
                g.bpm(),
                g.downbeat_at_or_after(0.0)
            ),
            Err(e) => println!("{path}: {e} ({len:.0} s of audio in {took:.2} s)"),
        }
    }
}
