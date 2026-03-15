/// Pure functions for insert cursor arrow-key navigation.
///
/// Matches the Unity InputHandler.cs arrow key behavior:
/// - Left/Right: move by grid step (Shift = 1/16 beat fine nudge)
/// - Up/Down: move to adjacent layer, skipping zero-height (collapsed) layers
/// - If a clip exists at the new position, auto-select it (Ableton behavior)
/// - Beat clamped >= 0, layer clamped to valid range

/// 1/16 beat — fine nudge step when Shift is held.
pub const FINE_NUDGE_BEATS: f32 = 1.0 / 16.0;

/// Direction of cursor navigation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// Info about a layer needed for navigation.
#[derive(Debug, Clone)]
pub struct NavLayerInfo {
    /// Layer index in the project.
    pub index: usize,
    /// Display height — 0.0 means collapsed/hidden (skip during Up/Down).
    pub height: f32,
}

/// Info about a clip for auto-select during navigation.
#[derive(Debug, Clone)]
pub struct NavClipInfo {
    pub clip_id: String,
    pub layer_index: usize,
    pub start_beat: f32,
    pub end_beat: f32,
}

/// Result of cursor navigation.
#[derive(Debug, Clone, PartialEq)]
pub enum NavResult {
    /// A clip was found at the new position — select it.
    SelectClip(String),
    /// No clip at position — set insert cursor here.
    SetCursor { beat: f32, layer: usize },
    /// Navigation was not possible (at boundary).
    NoChange,
}

/// Navigate the insert cursor in the given direction.
///
/// # Arguments
/// - `direction`: which arrow key was pressed
/// - `current_beat`: current cursor beat position
/// - `current_layer`: current cursor layer index
/// - `grid_interval`: current grid step (from `grid_interval_for_zoom`)
/// - `is_fine`: true when Shift is held (1/16 beat step for Left/Right)
/// - `layers`: all layers with height info (for Up/Down skipping collapsed)
/// - `clips`: all clips on the timeline (for auto-select)
pub fn navigate_cursor(
    direction: Direction,
    current_beat: f32,
    current_layer: usize,
    grid_interval: f32,
    is_fine: bool,
    layers: &[NavLayerInfo],
    clips: &[NavClipInfo],
) -> NavResult {
    match direction {
        Direction::Left | Direction::Right => {
            let step = if is_fine { FINE_NUDGE_BEATS } else { grid_interval };
            let delta = if direction == Direction::Left { -step } else { step };
            let new_beat = (current_beat + delta).max(0.0);

            // Check if a clip exists at the new position on the current layer
            if let Some(clip) = find_clip_at(clips, new_beat, current_layer) {
                NavResult::SelectClip(clip.clip_id.clone())
            } else {
                NavResult::SetCursor {
                    beat: new_beat,
                    layer: current_layer,
                }
            }
        }
        Direction::Up | Direction::Down => {
            let new_layer = find_adjacent_visible_layer(
                layers,
                current_layer,
                direction == Direction::Up,
            );

            match new_layer {
                Some(layer_idx) => {
                    // Check if a clip exists at current beat on the new layer
                    if let Some(clip) = find_clip_at(clips, current_beat, layer_idx) {
                        NavResult::SelectClip(clip.clip_id.clone())
                    } else {
                        NavResult::SetCursor {
                            beat: current_beat,
                            layer: layer_idx,
                        }
                    }
                }
                None => NavResult::NoChange,
            }
        }
    }
}

/// Find a clip that contains the given beat on the given layer.
fn find_clip_at<'a>(clips: &'a [NavClipInfo], beat: f32, layer: usize) -> Option<&'a NavClipInfo> {
    clips.iter().find(|c| {
        c.layer_index == layer && c.start_beat <= beat && c.end_beat > beat
    })
}

/// Find the next visible layer in the given direction, skipping zero-height layers.
fn find_adjacent_visible_layer(
    layers: &[NavLayerInfo],
    current: usize,
    going_up: bool,
) -> Option<usize> {
    if going_up {
        // Up = lower index (layers render bottom-to-top in Unity)
        if current == 0 {
            return None;
        }
        for i in (0..current).rev() {
            if layers.get(i).map_or(false, |l| l.height > 0.0) {
                return Some(i);
            }
        }
        None
    } else {
        // Down = higher index
        for i in (current + 1)..layers.len() {
            if layers.get(i).map_or(false, |l| l.height > 0.0) {
                return Some(i);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_layers(heights: &[f32]) -> Vec<NavLayerInfo> {
        heights
            .iter()
            .enumerate()
            .map(|(i, &h)| NavLayerInfo { index: i, height: h })
            .collect()
    }

    fn make_clip(id: &str, layer: usize, start: f32, dur: f32) -> NavClipInfo {
        NavClipInfo {
            clip_id: id.to_string(),
            layer_index: layer,
            start_beat: start,
            end_beat: start + dur,
        }
    }

    #[test]
    fn arrow_right_moves_by_grid_step() {
        let layers = make_layers(&[140.0]);
        let result = navigate_cursor(
            Direction::Right, 4.0, 0, 1.0, false, &layers, &[],
        );
        assert_eq!(result, NavResult::SetCursor { beat: 5.0, layer: 0 });
    }

    #[test]
    fn arrow_left_moves_by_grid_step() {
        let layers = make_layers(&[140.0]);
        let result = navigate_cursor(
            Direction::Left, 4.0, 0, 1.0, false, &layers, &[],
        );
        assert_eq!(result, NavResult::SetCursor { beat: 3.0, layer: 0 });
    }

    #[test]
    fn shift_arrow_moves_by_sixteenth() {
        let layers = make_layers(&[140.0]);
        let result = navigate_cursor(
            Direction::Right, 4.0, 0, 1.0, true, &layers, &[],
        );
        let expected_beat = 4.0 + FINE_NUDGE_BEATS;
        assert_eq!(result, NavResult::SetCursor { beat: expected_beat, layer: 0 });
    }

    #[test]
    fn arrow_left_clamps_to_zero() {
        let layers = make_layers(&[140.0]);
        let result = navigate_cursor(
            Direction::Left, 0.0, 0, 1.0, false, &layers, &[],
        );
        assert_eq!(result, NavResult::SetCursor { beat: 0.0, layer: 0 });
    }

    #[test]
    fn arrow_up_skips_collapsed_layers() {
        // Layer 0: visible, Layer 1: collapsed (height 0), Layer 2: visible (current)
        let layers = make_layers(&[140.0, 0.0, 140.0]);
        let result = navigate_cursor(
            Direction::Up, 4.0, 2, 1.0, false, &layers, &[],
        );
        // Should skip layer 1 (collapsed) and land on layer 0
        assert_eq!(result, NavResult::SetCursor { beat: 4.0, layer: 0 });
    }

    #[test]
    fn arrow_down_skips_collapsed_layers() {
        // Layer 0: visible (current), Layer 1: collapsed, Layer 2: visible
        let layers = make_layers(&[140.0, 0.0, 140.0]);
        let result = navigate_cursor(
            Direction::Down, 4.0, 0, 1.0, false, &layers, &[],
        );
        assert_eq!(result, NavResult::SetCursor { beat: 4.0, layer: 2 });
    }

    #[test]
    fn arrow_up_at_top_returns_no_change() {
        let layers = make_layers(&[140.0, 140.0]);
        let result = navigate_cursor(
            Direction::Up, 4.0, 0, 1.0, false, &layers, &[],
        );
        assert_eq!(result, NavResult::NoChange);
    }

    #[test]
    fn auto_selects_clip_at_position() {
        let layers = make_layers(&[140.0]);
        let clips = vec![make_clip("clip-a", 0, 5.0, 2.0)];
        let result = navigate_cursor(
            Direction::Right, 4.0, 0, 1.0, false, &layers, &clips,
        );
        // Beat moves to 5.0, clip-a spans [5.0, 7.0) — auto-select
        assert_eq!(result, NavResult::SelectClip("clip-a".to_string()));
    }

    #[test]
    fn sets_cursor_when_no_clip() {
        let layers = make_layers(&[140.0]);
        let clips = vec![make_clip("clip-a", 0, 10.0, 2.0)]; // Far away
        let result = navigate_cursor(
            Direction::Right, 4.0, 0, 1.0, false, &layers, &clips,
        );
        assert_eq!(result, NavResult::SetCursor { beat: 5.0, layer: 0 });
    }

    #[test]
    fn arrow_down_auto_selects_clip_on_new_layer() {
        let layers = make_layers(&[140.0, 140.0]);
        let clips = vec![make_clip("clip-b", 1, 3.0, 4.0)]; // Layer 1, spans [3, 7)
        let result = navigate_cursor(
            Direction::Down, 4.0, 0, 1.0, false, &layers, &clips,
        );
        // Moves to layer 1 at beat 4.0 — clip-b contains beat 4.0
        assert_eq!(result, NavResult::SelectClip("clip-b".to_string()));
    }
}
