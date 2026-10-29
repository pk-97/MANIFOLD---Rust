# Audio analysis tools

Offline Python for the trained kick and snare detectors. Detect on an audio clip runs in Rust
(`crates/manifold-playback/src/percussion_orchestrator.rs` over `manifold_audio::detectors::candidates`); nothing
here runs inside the app.

- `lab/`: label, train, compare and ship the detectors. Start with `lab/README.md`.
- `eval/`: label builders, label tools and the detector measures the lab reads.
- `catalog/`: the catalogue of Peter's Ableton sets and bounces that songs are picked from.
- `kick_container.py`: writes the model container the Rust reader (`crates/manifold-audio/src/trained/container.rs`)
  loads.
