# Project Map

## Core Structure

Workspace crates:
- `crates/manifold-core`: pure data models, enums, registries, math, timeline/project types
- `crates/manifold-editing`: commands, undo/redo, editing service, mutation gateway
- `crates/manifold-playback`: playback engine, scheduler, sync, live clips, renderer traits
- `crates/manifold-io`: load/save/migration and archive support
- `crates/manifold-renderer`: wgpu compositor, effects, generators, shaders
- `crates/manifold-ui`: bitmap UI, panels, tree, input, layout
- `crates/manifold-app`: winit application shell, bridge glue, app lifecycle

Dependency direction:
- `manifold-core` sits at the base
- `manifold-editing`, `manifold-playback`, `manifold-renderer`, `manifold-ui`, and `manifold-io` depend inward
- `manifold-app` is the outer integration layer

## Unity Source Root

- Unity project: `/Users/peterkiemann/MANIFOLD - Render Engine/`
- Rust repo: `/Users/peterkiemann/MANIFOLD - Rust/`

Translate directly from Unity source files, not from audits or prose docs.

## First Files to Check Before Porting

- `/Users/peterkiemann/MANIFOLD - Rust/docs/PORT_STATUS.md`
- `/Users/peterkiemann/MANIFOLD - Rust/docs/KNOWN_DIVERGENCES.md`
- `/Users/peterkiemann/MANIFOLD - Rust/docs/parity_tracker.json`

## High-Risk Drift Zones

- `crates/manifold-app/src/app.rs`
- `crates/manifold-app/src/ui_bridge.rs`
- effect and generator implementations under `crates/manifold-renderer/src/`
- any code path that duplicates project IO, clip scheduling, sync, or dialog behavior inline

## Frequent Validation Targets

- Ported pure logic: `cargo test -p manifold-core`, `cargo test -p manifold-editing`, `cargo test -p manifold-io`
- Renderer and app changes: `cargo check -p manifold-renderer`, `cargo check -p manifold-app`
- Broad sanity pass: `cargo check`
