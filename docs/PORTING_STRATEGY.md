# MANIFOLD Porting Strategy — Safe Behavioral Migration

> **Problem:** Complex dynamic interaction behaviors (drag, snap, click, scroll, undo commit patterns) are hard to verify through static analysis or manual visual inspection. How do we ensure the Rust port matches Unity exactly without repeatedly checking by hand?

---

## Strategy: Interaction Tests as the Source of Truth

The core insight is that most MANIFOLD interactions are **pure state transitions** — given an initial state and a sequence of inputs, the output state is deterministic. This means we can write **automated tests** that verify behavior without needing a running GPU or window.

### What Can Be Tested Without a Window

These are all pure logic and can be tested with unit/integration tests in Rust:

| Behavior | Test Approach |
|----------|--------------|
| Clip move (beat + layer change) | Simulate drag with (startBeat, startLayer) -> (endBeat, endLayer) |
| Clip trim (left/right, min duration, InPoint adjustment) | Simulate trim with start/end positions |
| Magnetic snap (grid + neighbor edges) | Unit test with known clip positions and zoom level |
| Grid snap at different zoom levels | Unit test with ppb -> grid interval mapping |
| Overlap enforcement (DaVinci-style) | Given placed clip + existing clips, assert correct trim/delete |
| Selection model (exclusivity, clearing rules) | Simulate click sequences, assert selection state |
| Undo commit patterns (snapshot/commit) | Simulate drag sequence, assert undo stack contents |
| Insert cursor navigation (arrow keys, auto-select) | Simulate key presses, assert cursor/selection state |
| Region selection (partial overlap inclusion) | Given region bounds + clip positions, assert selected set |
| Keyboard shortcuts (modifier matching, context-sensitive) | Simulate key combos, assert dispatched action |
| Escape priority chain | Set up each state level, press Escape, assert correct action |
| Cross-layer type checking | Move video clip to gen layer, assert blocked |
| Quarter-note snap on loop slider | Drag to fractional value, assert snapped |
| Clip creation (double-click, paste, duplicate) | Simulate action, assert new clip properties |
| Effect card selection (click, cmd+click, shift+click) | Simulate, assert selected indices |
| Effect clipboard (copy/cut/paste) | Copy effects, paste, assert new effect list |

### What Requires Visual Verification

These cannot be tested with pure logic tests and need manual or screenshot-based verification:

| Behavior | Why |
|----------|-----|
| Cursor icon changes (move/resize/blocked) | OS cursor API |
| Ghost element appearance during drag | GPU rendering |
| Drop indicator line position | GPU rendering |
| Slider fill/thumb visual position | GPU rendering |
| Clip color states (normal/hover/selected) | GPU rendering |
| Hover brightness change | GPU rendering |
| Effect card border color on selection | GPU rendering |

---

## Recommended Implementation Order

### Phase 1: Build the Test Harness

Create a test module in `manifold-app` (or a new `manifold-interaction-tests` crate) that can:

1. **Construct a test project** with known clips, layers, effects
2. **Simulate pointer events** (down, move, up at specific coordinates)
3. **Simulate keyboard events** (key + modifiers)
4. **Assert state** (clip positions, selection, undo stack, cursor state)

```rust
// Example test harness API
let mut ctx = TestContext::new();
ctx.add_layer(LayerType::Video);
ctx.add_clip(layer: 0, start: 4.0, duration: 2.0);
ctx.add_clip(layer: 0, start: 8.0, duration: 2.0);

// Simulate click on clip body at beat 5 (offset = 1 from clip start at 4)
ctx.pointer_down(beat: 5.0, layer: 0, hit: HitRegion::Body);
ctx.pointer_move(beat: 10.0, layer: 0);  // drag to beat 10
ctx.pointer_up(beat: 10.0, layer: 0);

// Assert
assert_eq!(ctx.clip(0).start_beat, 9.0);  // 10 - 1 offset
assert_eq!(ctx.undo_stack.len(), 1);
assert!(ctx.undo_stack[0].description().contains("Move"));
```

### Phase 2: Port Interaction Logic as Pure Functions

Extract interaction logic from UI panels into testable pure functions:

```rust
// magnetic_snap.rs — pure function, no UI dependency
pub fn magnetic_snap_beat(
    raw_beat: f32,
    grid_interval: f32,
    neighbor_edges: &[f32],  // start/end beats of clips on same layer
    ppb: f32,
    snap_threshold_px: f32,  // 12.0
    max_snap_beats: f32,     // 0.5
) -> f32 { ... }

// overlap_enforcement.rs — pure function
pub fn enforce_non_overlap(
    placed_clip: &ClipBounds,
    existing_clips: &[ClipBounds],
) -> Vec<OverlapAction> { ... }

// selection_model.rs — pure state machine
pub fn apply_click(
    state: &mut SelectionState,
    target: ClickTarget,
    modifiers: Modifiers,
) { ... }
```

### Phase 3: Write Tests from the Contract

Use `INTERACTION_CONTRACT.md` section 29 (Appendix: Recommended Testing Strategy) as the test plan. Each test case in that appendix maps to a Rust `#[test]` function.

### Phase 4: Implement UI Wiring

Once the pure logic passes all tests, wire it into the UI panels. The panels become thin dispatch layers that convert pointer events to pure function calls.

---

## Avoiding Manual Checking

### For Logic
- Every interaction behavior from `INTERACTION_CONTRACT.md` gets a corresponding `#[test]`
- Tests run in CI — regressions are caught immediately
- New interaction behaviors get a test BEFORE implementation (TDD)

### For Visuals
- Use **screenshot comparison** (render a frame, compare against reference PNG)
- Requires a headless wgpu context (wgpu supports this)
- Create reference screenshots from the Unity version
- Compare pixel-by-pixel with a tolerance threshold

### For Color Accuracy
- All color constants are in `manifold-ui/src/color.rs`
- Write a test that compares every Rust color constant against the Unity `UIConstants.cs` values
- This is a pure data test — no rendering needed:

```rust
#[test]
fn test_color_parity() {
    // From Unity UIConstants.cs
    assert_eq!(PLAY_ACTIVE, Color32::new(64, 184, 82, 255));   // #40B852
    assert_eq!(STOP_RED, Color32::new(128, 51, 51, 255));      // #803333
    assert_eq!(ACCENT_BLUE, Color32::new(89, 148, 235, 255));  // #5994EB
    // ... every color
}
```

### For Layout Dimensions
- Same approach — compare every constant:

```rust
#[test]
fn test_layout_parity() {
    assert_eq!(RULER_HEIGHT, 24.0);     // NOT 40.0
    assert_eq!(TRACK_HEIGHT, 140.0);
    assert_eq!(CLIP_VERTICAL_PADDING, 12.0);  // NOT 4.0
    // ... every dimension
}
```

---

## Summary

| What | How to Verify | Automated? |
|------|--------------|------------|
| Interaction state machines | Unit tests on pure functions | Yes |
| Snap/grid logic | Unit tests with known positions | Yes |
| Undo commit patterns | Integration tests on EditingService | Yes |
| Selection model | State machine tests | Yes |
| Colors | Constant comparison tests | Yes |
| Dimensions | Constant comparison tests | Yes |
| Visual appearance | Screenshot comparison | Semi (needs headless GPU) |
| Cursor changes | Manual check | No |
| Feel/timing | Manual check | No |

The goal: **95% of behavioral correctness is verifiable through automated tests on pure functions.** The remaining 5% (visual appearance, cursor icons, animation feel) requires manual verification but is much easier to check in isolation once the logic is known-correct.
