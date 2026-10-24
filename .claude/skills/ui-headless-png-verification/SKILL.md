---
name: ui-headless-png-verification
description: The custom Metal UI renders headlessly to a PNG you can Read — visual UI changes are self-verifiable without the running app. Invoke to verify any visual UI change.
---
The "renderer is custom, can't screenshot here" assumption is outdated. Visual UI changes CAN be self-verified headlessly: `GpuDevice::new()` runs windowless, `UIRenderer` rasterizes a `UITree` to a `GpuTexture`, readback → PNG, then `Read` the PNG.

Primary tool = the `ui-snap` harness (`crates/manifold-app/src/ui_snapshot/`, feature `ui-snapshot`). Run `cargo xtask ui-snap <scene>` (or `cargo run -p manifold-app --features ui-snapshot -- ui-snap <scene>`). PNG lands in `target/ui-snapshots/<scene>/`.

Scenes:
- `timeline` — whole timeline. Flags: `--interact "select:<layer>"`, `--dump`, `--thumbs`, `--vs-mockup`.
- `states` — 6-state matrix in one image.
- `inspector` — selected layer + real Mirror→Bloom chain + armed teal LFO drawer.
- `graph` — node-graph canvas + per-node output thumbnails. `--preset <TypeId>` (default Mirror).
- `editor` — full graph-editor window (preview left, card lane right). Generator presets only.
- `all` — renders timeline/states/inspector/graph/editor in one process.

Read the generated PNG in `target/ui-snapshots/<scene>/` to verify. Contact-sheet test: `crates/manifold-app/tests/renderer_contracts/ui_color_swatches.rs` (`SWATCH_OUT=<dir>`).
