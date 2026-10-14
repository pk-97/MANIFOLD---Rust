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
    (crossbeam unbounded)
                                      ContentState ──────────────►
                                      (one-slot handoff, newest state wins)
    GPU output ◄─────────────────     IOSurface triple-buffer
    (atomic front_index)              (zero-copy kernel memory)
```

**The content thread owns ALL mutable project state.** The UI thread gets read-only
`Arc<Project>` snapshots via `ContentState` when `data_version` changes, and the
content stamps (`PartStamps`) say which parts moved.

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

Every project write goes through `EditingService`, on both threads
(docs/UI_INVALIDATION_DESIGN.md D3, D5, D13, D24). Each write names the parts it
touches (`ChangeSet` of `Part`s); the stamps move, and the frame planner and the
content reactions read the stamps. There is no other signal.

```rust
// Undoable: a command. Command::touches has no default; every command
// declares its parts.
ContentCommand::Execute(Box::new(MyCommand { ... }))
// → EditingService::execute → UndoRedoManager → Command::execute

// Non-undoable: one MutateProject with a required change set.
ContentCommand::mutate(touch::VALUES, Box::new(|project: &mut Project| { ... }))
// → EditingService::mutate(touches, project, f)
```

- `touch::*` in `content_command.rs` names the common sets. A live value write
  declares `Values`; a live drag of a modulator, gain, crossover or mapping
  shape declares `Shapes`. Neither has reactions, so drags stay cheap. A closure
  that cannot name its parts should be a command.
- Content side: the engine's only mutable entry is `PlaybackEngine::project_edit()`
  → `ProjectEdit`, a `manifold-editing` newtype with no public way back to
  `&mut Project`; only `EditingService` (execute, undo, redo, record, `mutate`)
  and `tick_write` consume it.
- Tick writes: the five `TickField`s (BPM, clock authority, session tempo
  points, recording provenance, base tempo) go through
  `manifold_editing::tick_write`: compare first, no stamp, no publish, no
  reaction. Anything structural the tick produces returns a command.
- UI side: `ctx.project` is read-only; write the local copy only through
  `DispatchCtx::mutate(touches, f)` / `execute(cmd)` (`ui_bridge/context.rs`),
  which apply locally, stamp, and send the same edit to the content thread.
- Every write compares before it mutates, so an unchanged value moves no stamp.

**NEVER:**
- Mutate project state directly from UI, or open `&mut Project` around the gateway
- Add an invalidation flag; the frame planner decides UI work from its inputs
- Create new `Arc<Mutex<>>` shared state without approval
- Write to `param_values` (overwritten every tick) — write to `base_param_values`
- Add allocations to `engine_tick()` or `sync_clips_to_time()`

## ContentCommand → UI Flow

```
UI click → DispatchCtx::execute / mutate (local write + stamp) → send ContentCommand
Content thread drains all → EditingService → stamps move → reactions run once per drain
Content thread publishes ContentState (one slot; a replaced state folds into the newest)
UI accepts the snapshot → moved parts bump its own stamps → the frame planner
projects, rebuilds or pushes in place from its inputs
```

## Key Types

```rust
// Commands (manifold-app/src/content_command.rs)
enum ContentCommand {
    Execute(Box<dyn Command>),
    MutateProject { touches: ChangeSet, f: Box<dyn FnOnce(&mut Project) + Send> },
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
| UI→Content | ContentCommand | crossbeam unbounded | logs on disconnect |
| Content→UI | ContentState | one-slot handoff (`state_channel`) | UI takes one per frame; a replaced state folds into the newest |
| GPU output | IOSurface | Triple-buffer | Atomic front_index, zero-copy |
| OSC→Content | PendingWrite | Arc<Mutex<Vec>> | Only shared mutable state |
