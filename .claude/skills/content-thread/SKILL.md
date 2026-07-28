---
name: content-thread
description: How the two-thread model works — mutation flow, state sync, what NOT to do. Invoke before touching ContentThread, ContentCommand, or state snapshots.
---
# Content Thread Architecture Guide

## The Two-Thread Model

```
UI Thread (winit event loop)          Content Thread (project FPS)
──────────────────────────           ─────────────────────────────
Input handling                        PlaybackEngine
UI rendering (manifold-gpu)            EditingService
Presents GPU output                   ContentPipeline (generators + effects)
                                      ClipScheduler
    ContentCommand ─────────────►     Sync (MIDI/OSC/Link)
    (crossbeam bounded=64)
                                      ContentState ──────────────►
                                      (crossbeam bounded=4)
    GPU output ◄─────────────────     IOSurface triple-buffer
    (atomic front_index)              (zero-copy kernel memory)
```

**The content thread owns ALL mutable project state.** The UI thread gets read-only
`Arc<Project>` snapshots via `ContentState` only when `data_version` changes.

## Content Thread Frame Loop

```
1. Wait for VSync signal (condvar from CVDisplayLink)
2. Drain all pending ContentCommands
3. engine_tick():
   a. sync_clips_to_time() — SOLE authority for playback state
   b. ClipScheduler evaluates which clips are active
   c. TickResult: started/stopped clips, active clip list
4. ContentPipeline::render():
   a. Per-layer: generator.render() → effect chain
   b. Compositor blends all layers
   c. Output to IOSurface
5. Publish ContentState (Arc<Project> snapshot + data_version)
6. GPU completion handler publishes front_index
```

## Mutation Flow

ALL project mutations must go through one of these paths:

```rust
// Path 1: Undoable command (preferred for user actions)
ContentCommand::Execute(Box::new(MyCommand { ... }))
// → EditingService → UndoRedoManager → Command::execute(&mut Project)

// Path 2: Direct mutation (for non-undoable state changes)
ContentCommand::MutateProject(Box::new(|project: &mut Project| {
    project.settings.bpm = Bpm(120.0);
}))
```

**NEVER:**
- Mutate project state directly from UI
- Create new `Arc<Mutex<>>` shared state without approval
- Write to `param_values` (overwritten every tick) — write to `base_param_values`
- Add allocations to `engine_tick()` or `sync_clips_to_time()`

## ContentCommand → UI Flow

```
UI click → build ContentCommand → try_send (bounded=64, logs on full)
Content thread drains all → executes → increments data_version
Content thread publishes ContentState (bounded=4, try_send, UI drains all + keeps latest)
UI reads Arc<Project> snapshot → updates panel state
```

**DataVersion pattern:** Content thread increments `data_version` counter on every project
mutation. UI thread compares its cached version — only updates UI panels when changed.

## Key Types

```rust
// Commands (manifold-app/src/content_command.rs)
enum ContentCommand {
    Execute(Box<dyn Command>),
    MutateProject(Box<dyn FnOnce(&mut Project) + Send>),
    Transport(TransportCommand),
    // ... more variants
}

// State broadcast (manifold-app/src/content_state.rs)
struct ContentState {
    pub project: Arc<Project>,
    pub data_version: u64,
    pub playback_state: PlaybackState,
    pub beat_position: Beats,
    // ... more fields
}
```

## UI Bridge Modules

The UI bridge translates between UI actions and ContentCommands:

```
manifold-app/src/ui_bridge/
├── mod.rs          — UIBridge struct, dispatch
├── transport.rs    — play/stop/seek/tempo
├── editing.rs      — clip operations, selection
├── inspector.rs    — parameter changes
├── layer.rs        — layer add/remove/reorder
├── project.rs      — settings, save/load
└── state_sync.rs   — ContentState → UI panel updates
```

## Performance Rules

- **No per-frame allocations** on hot paths (engine_tick, sync, render)
- Pre-allocated scratch buffers: `stopped_this_tick`, `timeline_active_scratch`
- `AHashMap` for all clip/effect/generator ID lookups
- Static comparison functions for sorting (no per-frame closures)
- `parking_lot::RwLock/Mutex` — no poisoning, smaller, faster than std

## Thread Boundary Summary

| Direction | Type | Channel | Notes |
|-----------|------|---------|-------|
| UI→Content | ContentCommand | crossbeam bounded(64) | try_send, logs on full |
| Content→UI | ContentState | crossbeam bounded(4) | UI drains all, keeps latest |
| GPU output | IOSurface | Triple-buffer | Atomic front_index, zero-copy |
| OSC→Content | PendingWrite | Arc<Mutex<Vec>> | Only shared mutable state |
