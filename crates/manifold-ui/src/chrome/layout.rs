//! The layout engine — a pure mini-flexbox over [`View`] trees.
//!
//! [`solve`] resolves a `View` and a root `Rect` into a flat `Vec<LaidNode>` in
//! DFS pre-order (parent before children), each carrying its resolved bounds
//! plus the render attributes the [`diff`](crate::chrome::diff) reconciler needs.
//! No [`UITree`](crate::tree::UITree) dependency — measurement comes through a
//! [`TextMeasure`], so the whole engine is unit-tested headlessly.
//!
//! ## Model
//!
//! Each axis resolves independently: `Fixed` takes its size, `Hug` shrink-wraps
//! to content, `Fill` grows into the parent's leftover (split among siblings on
//! the main axis, stretched on the cross axis). `main_align` distributes
//! leftover main-axis space when nothing fills it; `cross_align` places each
//! child on the cross axis at its own size. `Stack`/`Leaf` place every child in
//! the padded box (horizontal by `main_align`, vertical by `cross_align`).

use crate::chrome::view::{Align, Layout, SliderSpec, Sizing, View, ViewIntent};
use crate::node::{Rect, UINodeType, UIStyle, Vec2};
use crate::text::TextMeasure;

/// One resolved node: bounds plus everything the reconciler writes to the tree.
/// Emitted in DFS pre-order; `parent` indexes earlier into the same `Vec`.
#[derive(Clone)]
pub struct LaidNode {
    pub rect: Rect,
    pub parent: Option<usize>,
    pub kind: UINodeType,
    pub style: UIStyle,
    pub text: Option<String>,
    pub interactive: bool,
    pub clips: bool,
    pub visible: bool,
    pub disabled: bool,
    pub intent: ViewIntent,
    /// Slider spec copied from [`View::slider_row`] — the host materialises a
    /// `BitmapSlider` into this node's `rect` when present.
    pub slider: Option<Box<SliderSpec>>,
    /// Stable-identity hint copied from [`View::key`]. The reconciler keys on
    /// structure, not this; it exists so a panel can resolve a specific node id
    /// for overlay anchoring (the "stable semantic addressing" the Chrome API
    /// gives panels in place of hand-stored `self.*_id` fields).
    pub key: Option<u64>,
    /// Opt-in durable-WidgetId pin copied from [`View::identity`] — the host
    /// mints this node via `add_node_keyed` (D4 card-root identity).
    pub identity: Option<u64>,
    /// Automation component name copied from [`View::name`] — applied to the
    /// built node via `UITree::set_name` (`UI_AUTOMATION_DESIGN.md` D8/§3).
    pub name: Option<&'static str>,
}

/// Resolve `root` within `rect`, returning every node laid out in DFS pre-order.
pub fn solve(root: &View, rect: Rect, measure: &dyn TextMeasure) -> Vec<LaidNode> {
    let mut out = Vec::new();
    solve_into(root, rect, measure, &mut out);
    out
}

/// [`solve`] into a caller-owned buffer (cleared first) — lets a panel host
/// reuse one allocation across frames.
pub fn solve_into(root: &View, rect: Rect, measure: &dyn TextMeasure, out: &mut Vec<LaidNode>) {
    out.clear();
    place(root, rect, None, measure, out);
}

/// Content (hug) size of a view's subtree along both axes — a leaf's measured
/// text, or a container's children laid end-to-end with gaps and padding.
fn measure_hug(view: &View, measure: &dyn TextMeasure) -> Vec2 {
    if view.children.is_empty() {
        // Leaf: text nodes hug their glyphs; everything else is zero.
        return match &view.text {
            Some(t) if !t.is_empty() => {
                measure.measure_text(t, view.style.font_size, view.style.font_weight)
            }
            _ => Vec2::ZERO,
        };
    }

    let n = view.children.len();
    let gap_total = if n > 1 { view.gap * (n - 1) as f32 } else { 0.0 };

    // Natural size of a child on one axis: Fixed takes its value, Hug/Fill both
    // contribute the child's content extent (Fill's growth only applies once
    // space is offered during placement).
    let child_main = |c: &View, horizontal: bool| -> f32 {
        let sizing = if horizontal { c.width } else { c.height };
        match sizing {
            Sizing::Fixed(v) => v,
            Sizing::Hug | Sizing::Fill => {
                let h = measure_hug(c, measure);
                if horizontal { h.x } else { h.y }
            }
        }
    };

    let (mut content_w, mut content_h) = (0.0_f32, 0.0_f32);
    match view.layout {
        Layout::Row => {
            for c in &view.children {
                content_w += child_main(c, true);
                content_h = content_h.max(child_main(c, false));
            }
            content_w += gap_total;
        }
        Layout::Column => {
            for c in &view.children {
                content_h += child_main(c, false);
                content_w = content_w.max(child_main(c, true));
            }
            content_h += gap_total;
        }
        Layout::Stack | Layout::Leaf => {
            for c in &view.children {
                content_w = content_w.max(child_main(c, true));
                content_h = content_h.max(child_main(c, false));
            }
        }
    }

    Vec2::new(
        content_w + view.pad.horizontal(),
        content_h + view.pad.vertical(),
    )
}

/// Offset of a child within `slack` free space, per alignment.
fn align_offset(slack: f32, align: Align) -> f32 {
    match align {
        Align::Start => 0.0,
        Align::Center => slack * 0.5,
        Align::End => slack,
    }
}

/// Resolve a child's cross-axis position+size given the container's cross span.
fn cross_place(sizing: Sizing, natural: f32, start: f32, avail: f32, align: Align) -> (f32, f32) {
    match sizing {
        Sizing::Fill => (start, avail),
        Sizing::Fixed(v) => (start + align_offset(avail - v, align), v),
        Sizing::Hug => (start + align_offset(avail - natural, align), natural),
    }
}

fn place(view: &View, rect: Rect, parent: Option<usize>, measure: &dyn TextMeasure, out: &mut Vec<LaidNode>) {
    let me = out.len();
    out.push(LaidNode {
        rect,
        parent,
        kind: view.kind,
        style: view.style,
        text: view.text.clone(),
        interactive: view.interactive,
        clips: view.clips,
        visible: view.visible,
        disabled: view.disabled,
        intent: view.intent.clone(),
        slider: view.slider.clone(),
        key: view.key,
        identity: view.identity,
        name: view.name,
    });

    if view.children.is_empty() {
        return;
    }

    let inner = Rect::new(
        rect.x + view.pad.l,
        rect.y + view.pad.t,
        (rect.width - view.pad.horizontal()).max(0.0),
        (rect.height - view.pad.vertical()).max(0.0),
    );

    match view.layout {
        Layout::Row => place_linear(view, inner, true, me, measure, out),
        Layout::Column => place_linear(view, inner, false, me, measure, out),
        Layout::Stack | Layout::Leaf => {
            for child in &view.children {
                let nat = measure_hug(child, measure);
                let (x, w) = cross_place(child.width, nat.x, inner.x, inner.width, view.main_align);
                let (y, h) = cross_place(child.height, nat.y, inner.y, inner.height, view.cross_align);
                place(child, Rect::new(x, y, w, h), Some(me), measure, out);
            }
        }
    }
}

/// Place children along a main axis (`horizontal` = Row; else Column).
fn place_linear(
    view: &View,
    inner: Rect,
    horizontal: bool,
    me: usize,
    measure: &dyn TextMeasure,
    out: &mut Vec<LaidNode>,
) {
    let n = view.children.len();
    let avail_main = if horizontal { inner.width } else { inner.height };
    let gap_total = if n > 1 { view.gap * (n - 1) as f32 } else { 0.0 };

    // First pass: main-axis size of each child; Fill children deferred.
    let mut main_size = vec![0.0_f32; n];
    let mut fill_count = 0usize;
    let mut used = 0.0_f32;
    for (i, child) in view.children.iter().enumerate() {
        let sizing = if horizontal { child.width } else { child.height };
        match sizing {
            Sizing::Fixed(v) => {
                main_size[i] = v;
                used += v;
            }
            Sizing::Hug => {
                let nat = measure_hug(child, measure);
                let s = if horizontal { nat.x } else { nat.y };
                main_size[i] = s;
                used += s;
            }
            Sizing::Fill => fill_count += 1,
        }
    }

    let leftover = avail_main - used - gap_total;
    let mut start_offset = 0.0_f32;
    if fill_count > 0 {
        let each = (leftover / fill_count as f32).max(0.0);
        for (i, child) in view.children.iter().enumerate() {
            let sizing = if horizontal { child.width } else { child.height };
            if sizing == Sizing::Fill {
                main_size[i] = each;
            }
        }
    } else if leftover > 0.0 {
        start_offset = align_offset(leftover, view.main_align);
    }

    // Second pass: position along main, resolve cross.
    let (cross_start, cross_avail) = if horizontal {
        (inner.y, inner.height)
    } else {
        (inner.x, inner.width)
    };
    let main_start = if horizontal { inner.x } else { inner.y };
    let mut cursor = main_start + start_offset;
    for (i, child) in view.children.iter().enumerate() {
        let nat = measure_hug(child, measure);
        let child_rect = if horizontal {
            let (y, h) = cross_place(child.height, nat.y, cross_start, cross_avail, view.cross_align);
            Rect::new(cursor, y, main_size[i], h)
        } else {
            let (x, w) = cross_place(child.width, nat.x, cross_start, cross_avail, view.cross_align);
            Rect::new(x, cursor, w, main_size[i])
        };
        place(child, child_rect, Some(me), measure, out);
        cursor += main_size[i] + view.gap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chrome::view::Pad;
    use crate::node::FontWeight;

    /// Deterministic measurer: every glyph is 10×16. Keeps geometry exact.
    struct Mono;
    impl TextMeasure for Mono {
        fn measure_text(&self, text: &str, _font_size: u16, _weight: FontWeight) -> Vec2 {
            Vec2::new(text.chars().count() as f32 * 10.0, 16.0)
        }
    }

    fn r(x: f32, y: f32, w: f32, h: f32) -> Rect {
        Rect::new(x, y, w, h)
    }

    #[test]
    fn row_splits_fill_equally() {
        // Two Fill-width buttons across a 100-wide row, gap 0 → 50 each. Height
        // is Hug (default) → each hugs its 16-tall text, placed at cross Start.
        let v = View::row(0.0)
            .child(View::button("a").fill_w().inert())
            .child(View::button("b").fill_w().inert());
        let laid = solve(&v, r(0.0, 0.0, 100.0, 20.0), &Mono);
        assert_eq!(laid.len(), 3);
        assert_eq!(laid[1].rect, r(0.0, 0.0, 50.0, 16.0));
        assert_eq!(laid[2].rect, r(50.0, 0.0, 50.0, 16.0));
    }

    #[test]
    fn cross_fill_stretches_to_container() {
        // Fill on the cross axis (height) stretches the child to the row height.
        let v = View::row(0.0).child(View::button("a").fill().inert());
        let laid = solve(&v, r(0.0, 0.0, 100.0, 20.0), &Mono);
        assert_eq!(laid[1].rect, r(0.0, 0.0, 100.0, 20.0));
    }

    #[test]
    fn row_fixed_then_fill_takes_remainder() {
        // Fixed 30 + gap 10 + Fill → Fill = 100 - 30 - 10 = 60.
        let v = View::row(10.0)
            .child(View::panel().w(Sizing::Fixed(30.0)).h(Sizing::Fixed(20.0)))
            .child(View::panel().fill_w().h(Sizing::Fixed(20.0)));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 20.0), &Mono);
        assert_eq!(laid[1].rect, r(0.0, 0.0, 30.0, 20.0));
        assert_eq!(laid[2].rect, r(40.0, 0.0, 60.0, 20.0));
    }

    #[test]
    fn spacer_pushes_sibling_to_end() {
        // label | spacer | label → second label pinned to the right edge.
        let v = View::row(0.0)
            .child(View::label("L").w(Sizing::Fixed(10.0)).h(Sizing::Fixed(16.0)))
            .child(View::spacer())
            .child(View::label("R").w(Sizing::Fixed(10.0)).h(Sizing::Fixed(16.0)));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 16.0), &Mono);
        assert_eq!(laid[1].rect.x, 0.0);
        assert_eq!(laid[3].rect.x, 90.0, "right label pinned to edge");
    }

    #[test]
    fn column_stacks_with_gap() {
        let v = View::column(5.0)
            .child(View::panel().fill_w().h(Sizing::Fixed(20.0)))
            .child(View::panel().fill_w().h(Sizing::Fixed(30.0)));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 200.0), &Mono);
        assert_eq!(laid[1].rect, r(0.0, 0.0, 100.0, 20.0));
        assert_eq!(laid[2].rect, r(0.0, 25.0, 100.0, 30.0));
    }

    #[test]
    fn hug_column_sizes_to_content() {
        // Column hug height = 16 + 16 + gap 4 + pad 2*2 = 40; width = 30 + pad 4.
        let inner = View::column(4.0)
            .pad(Pad::all(2.0))
            .child(View::label("abc")) // 30×16
            .child(View::label("x")); // 10×16
        let measured = measure_hug(&inner, &Mono);
        assert_eq!(measured, Vec2::new(34.0, 40.0));
    }

    #[test]
    fn pad_insets_children() {
        let v = View::column(0.0)
            .pad(Pad::all(8.0))
            .child(View::panel().fill());
        let laid = solve(&v, r(0.0, 0.0, 100.0, 100.0), &Mono);
        assert_eq!(laid[1].rect, r(8.0, 8.0, 84.0, 84.0));
    }

    #[test]
    fn cross_center_aligns() {
        // A 20-tall fixed child centered in a 60-tall row → y = 20.
        let v = View::row(0.0)
            .cross_align(Align::Center)
            .child(View::panel().fixed(10.0, 20.0));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 60.0), &Mono);
        assert_eq!(laid[1].rect.y, 20.0);
        assert_eq!(laid[1].rect.height, 20.0);
    }

    #[test]
    fn main_align_end_packs_right() {
        // One fixed 30 child, End-aligned in 100 → x = 70.
        let v = View::row(0.0)
            .main_align(Align::End)
            .child(View::panel().fixed(30.0, 20.0));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 20.0), &Mono);
        assert_eq!(laid[1].rect.x, 70.0);
    }

    #[test]
    fn dfs_preorder_with_parents() {
        let v = View::column(0.0)
            .child(View::row(0.0).child(View::label("a")).child(View::label("b")))
            .child(View::label("c"));
        let laid = solve(&v, r(0.0, 0.0, 100.0, 100.0), &Mono);
        // root, row, a, b, c
        assert_eq!(laid.len(), 5);
        assert_eq!(laid[0].parent, None);
        assert_eq!(laid[1].parent, Some(0)); // row
        assert_eq!(laid[2].parent, Some(1)); // a
        assert_eq!(laid[3].parent, Some(1)); // b
        assert_eq!(laid[4].parent, Some(0)); // c
    }
}
