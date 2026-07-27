
# UI invariants & taste

## Layout invariants
- All layer Y offsets and heights come from CoordinateMapper — never computed inline (inline shortcuts have diverged from the viewport before).
- `layout.track_header_height()` is the single source for the Y offset between timeline body and scrollable tracks — viewport.rs and layer_header.rs must both use it.
- `generate_layers` requires clips sorted descending by layer_index — broken multiple times, always the same symptom: bottom layer appears on top.

## State invariants
- `TimelineClip.layer_id` is skip_serializing — empty after project load; layer index flows through the TickResult pipeline.
- Transport panel state (SYNC, CLK, LINK) updates via `ui_bridge/state_sync.rs` — the two-thread model has no polling/caching layer for UI state.
- When extracting a once-singleton UI resource into a per-window field, audit ALL write sites, not just reads — single-window code wrote to "the offscreen" from any window's event.

## Taste (settled with Peter)
- No conditionally visible UI — elements are permanently present or removed, never "sometimes there".
- High-saturation/high-contrast identity colors stay; never desaturate to match a muted mockup.
- Full-saturation layer headers stay (performer targeting); blue's dual role (selection + active) stays — the only fix is a white selection ring on headers for blue layers.
