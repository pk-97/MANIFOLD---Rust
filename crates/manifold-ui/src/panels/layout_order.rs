//! Relative-order checks for panel layout tests. They pin where a control sits
//! relative to its neighbours, never its pixel position, so spacing and size
//! tweaks don't break them while a reorder or a collision does.

use crate::node::Rect;

const EPS: f32 = 0.01;

/// `cells` are in the expected left-to-right order: each ends at or before the
/// next begins, and every cell sits horizontally inside `bounds`.
pub(crate) fn assert_left_to_right(cells: &[(&str, Rect)], bounds: Rect) {
    for (name, r) in cells {
        assert!(
            r.x >= bounds.x - EPS && r.x_max() <= bounds.x_max() + EPS,
            "{name} {r:?} escapes {bounds:?}"
        );
    }
    for pair in cells.windows(2) {
        let ((a, ra), (b, rb)) = (pair[0], pair[1]);
        assert!(ra.x_max() <= rb.x + EPS, "{a} {ra:?} must end before {b} {rb:?} starts");
    }
}

/// `cells` are in the expected top-to-bottom order: each ends at or before the
/// next begins, and every cell sits vertically inside `bounds`.
pub(crate) fn assert_top_to_bottom(cells: &[(&str, Rect)], bounds: Rect) {
    for (name, r) in cells {
        assert!(
            r.y >= bounds.y - EPS && r.y_max() <= bounds.y_max() + EPS,
            "{name} {r:?} escapes {bounds:?}"
        );
    }
    for pair in cells.windows(2) {
        let ((a, ra), (b, rb)) = (pair[0], pair[1]);
        assert!(ra.y_max() <= rb.y + EPS, "{a} {ra:?} must end above {b} {rb:?}");
    }
}

/// Every cell shares one row: each vertical span overlaps the first cell's.
pub(crate) fn assert_one_row(cells: &[(&str, Rect)]) {
    let (first, fr) = cells[0];
    for (name, r) in &cells[1..] {
        assert!(r.y < fr.y_max() && fr.y < r.y_max(), "{name} {r:?} is off {first}'s row {fr:?}");
    }
}

/// `inner` is horizontally centred in `outer`, to half a pixel.
pub(crate) fn assert_centred(what: &str, inner_x: f32, inner_x_max: f32, outer: Rect) {
    let mid = (inner_x + inner_x_max) * 0.5;
    let want = outer.x + outer.width * 0.5;
    assert!((mid - want).abs() < 0.5, "{what} centre {mid} != panel centre {want}");
}
