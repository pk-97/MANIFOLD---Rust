//! `View` — the immutable description a panel emits each frame.
//!
//! A panel builds a `View` tree from its own state; the [`layout`] engine
//! resolves it to rects and the [`diff`] reconciler applies it to the
//! [`UITree`](crate::tree::UITree). Builders are fluent and consume `self`, so a
//! whole panel reads as one nested expression. See `docs/CHROME_API_DESIGN.md`.
//!
//! [`layout`]: crate::chrome::layout
//! [`diff`]: crate::chrome::diff

#[cfg(test)]
use crate::{TransportAction};
use crate::node::{Color32, FontWeight, TextAlign, UINodeType, UIStyle};
use crate::panels::PanelAction;
use crate::slider::SliderColors;

/// A declarative slider — a typed Chrome building block. A panel puts a
/// [`View::slider_row`] carrying this in its description; the
/// [`ChromeHost`](crate::chrome::ChromeHost) *materialises* the multi-node
/// [`BitmapSlider`](crate::slider::BitmapSlider) into the laid row (byte-identical
/// geometry) and hands back its node ids, so the panel never hand-rolls the
/// slot + build itself. The live value/drag stays with the panel's
/// `SliderDragState` (the host owns the slider's *structure*, the panel its
/// *value*), which is why the spec only needs the build-time appearance.
#[derive(Clone)]
pub struct SliderSpec {
    /// Optional leading label (right-aligned); `None` for a bare inline slider.
    pub label: Option<String>,
    /// Normalised position 0–1 the slider is first drawn at.
    pub value: f32,
    /// Normalised position 0–1 the slider resets to on right-click (BUG-061).
    pub default: f32,
    /// Value-cell text (already formatted).
    pub value_text: String,
    pub colors: SliderColors,
    pub font_size: u16,
    /// Leading-label column width (0 when there is no label).
    pub label_width: f32,
    /// Right-click reset action fired on the slider's track (BUG-070 follow-
    /// through) — required so a chrome-host slider can never be materialised
    /// without stating its reset. The host stores it alongside the slider's
    /// ids and replays it via [`ChromeHost::register_slider_resets`].
    pub reset: PanelAction,
}

/// How a [`View`] sizes along one axis. Resolved independently per axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sizing {
    /// Exactly this many logical pixels.
    Fixed(f32),
    /// Shrink-wrap to content: a leaf hugs its measured text; a container hugs
    /// its laid-out children plus padding and gaps.
    Hug,
    /// Grow to the space the parent offers — split equally among sibling
    /// `Fill`s on the main axis, stretched to the container on the cross axis.
    Fill,
}

/// Alignment of children within a container's free space (per axis).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Align {
    #[default]
    Start,
    Center,
    End,
}

/// Padding inside a container, in logical pixels.
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Pad {
    pub l: f32,
    pub t: f32,
    pub r: f32,
    pub b: f32,
}

impl Pad {
    pub const ZERO: Pad = Pad { l: 0.0, t: 0.0, r: 0.0, b: 0.0 };

    pub fn all(v: f32) -> Self {
        Self { l: v, t: v, r: v, b: v }
    }

    /// Horizontal `x` on left+right, vertical `y` on top+bottom.
    pub fn xy(x: f32, y: f32) -> Self {
        Self { l: x, t: y, r: x, b: y }
    }

    pub fn horizontal(&self) -> f32 {
        self.l + self.r
    }

    pub fn vertical(&self) -> f32 {
        self.t + self.b
    }
}

/// How a container arranges its children.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Layout {
    /// No child arrangement (a leaf, or a single-child wrapper). Children, if
    /// any, are placed filling the padded box — used for one-child insets.
    Leaf,
    /// Children left-to-right; `gap` between them.
    Row,
    /// Children top-to-bottom; `gap` between them.
    Column,
    /// Children overlaid (z-stack); each placed in the padded box.
    Stack,
}

/// Per-node gesture intent — which [`PanelAction`] each discrete gesture fires.
/// Mirrors [`crate::intent::NodeIntent`]; the host copies it into the registry.
#[derive(Default, Clone)]
pub struct ViewIntent {
    pub click: Option<PanelAction>,
    pub double_click: Option<PanelAction>,
    pub right_click: Option<PanelAction>,
    /// Claims the node's whole area for intent fold-up (padding/gaps resolve
    /// here instead of falling through). See [`crate::intent::NodeIntent`].
    pub claims_area: bool,
}

impl ViewIntent {
    fn is_set(&self) -> bool {
        self.click.is_some()
            || self.double_click.is_some()
            || self.right_click.is_some()
            || self.claims_area
    }
}

/// An immutable node description. Built via the fluent constructors
/// ([`View::row`], [`View::label`], …) and modifiers ([`View::fill_w`],
/// [`View::on_click`], …), each consuming and returning `self`.
pub struct View {
    pub(crate) kind: UINodeType,
    pub(crate) style: UIStyle,
    pub(crate) text: Option<String>,
    pub(crate) width: Sizing,
    pub(crate) height: Sizing,
    pub(crate) layout: Layout,
    pub(crate) gap: f32,
    pub(crate) pad: Pad,
    pub(crate) main_align: Align,
    pub(crate) cross_align: Align,
    pub(crate) children: Vec<View>,
    pub(crate) intent: ViewIntent,
    pub(crate) clips: bool,
    pub(crate) interactive: bool,
    pub(crate) inert: bool,
    pub(crate) disabled: bool,
    pub(crate) visible: bool,
    /// Optional stable-identity hint (currently advisory — the diff keys on
    /// structural shape; a future keyed reconciler reads this).
    pub(crate) key: Option<u64>,
    /// Opt-in durable-WidgetId pin (D4, `docs/WIDGET_TREE_DESIGN.md`): when
    /// set, the host mints this node via `add_node_keyed`, so its identity
    /// survives sibling reorder (card roots in an effect chain). Distinct
    /// from `key`, which is lookup-only and merely host-unique — hosts can
    /// share a tree parent, so threading `key` into WidgetId salts would
    /// collide across panels. Identity values must be globally derived
    /// (`param_surface::stable_key` over a real id), never small constants.
    pub(crate) identity: Option<u64>,
    /// Automation component name (`UI_AUTOMATION_DESIGN.md` D8/§3), registered
    /// on the built `NodeId` via `UITree::set_name` once this view lands in the
    /// tree. `None` for the overwhelming majority of views — set only at the
    /// naming pass' high-value points (`.name("layer_header.mute")`).
    pub(crate) name: Option<&'static str>,
    /// When set, this node is a slider *slot*: the host materialises a
    /// [`BitmapSlider`](crate::slider::BitmapSlider) into its laid rect and
    /// records the resulting ids under [`View::key`].
    pub(crate) slider: Option<Box<SliderSpec>>,
}

impl View {
    fn bare(kind: UINodeType) -> Self {
        Self {
            kind,
            style: UIStyle::default(),
            text: None,
            width: Sizing::Hug,
            height: Sizing::Hug,
            layout: Layout::Leaf,
            gap: 0.0,
            pad: Pad::ZERO,
            main_align: Align::Start,
            cross_align: Align::Start,
            children: Vec::new(),
            intent: ViewIntent::default(),
            clips: false,
            interactive: false,
            inert: false,
            disabled: false,
            visible: true,
            key: None,
            identity: None,
            name: None,
            slider: None,
        }
    }

    // ── Constructors ────────────────────────────────────────────────

    /// A non-interactive rectangle (background, container, spacer base).
    pub fn panel() -> Self {
        Self::bare(UINodeType::Panel)
    }

    /// A horizontal container. `gap` separates children.
    pub fn row(gap: f32) -> Self {
        let mut v = Self::bare(UINodeType::Panel);
        v.layout = Layout::Row;
        v.gap = gap;
        v
    }

    /// A vertical container. `gap` separates children.
    pub fn column(gap: f32) -> Self {
        let mut v = Self::bare(UINodeType::Panel);
        v.layout = Layout::Column;
        v.gap = gap;
        v
    }

    /// A z-stack container — children overlaid in the padded box.
    pub fn stack() -> Self {
        let mut v = Self::bare(UINodeType::Panel);
        v.layout = Layout::Stack;
        v
    }

    /// A text label (non-interactive). Hugs its measured text by default.
    pub fn label(text: impl Into<String>) -> Self {
        let mut v = Self::bare(UINodeType::Label);
        v.text = Some(text.into());
        v
    }

    /// An interactive button carrying `text`. Requires an intent or `.inert()`
    /// (validation flags an unwired button).
    pub fn button(text: impl Into<String>) -> Self {
        let mut v = Self::bare(UINodeType::Button);
        v.text = Some(text.into());
        v.interactive = true;
        v
    }

    /// An interactive slider track. Drag is handled in `handle_event`, so a bare
    /// slider is typically `.inert()`; a click-to-act slider carries an intent.
    pub fn slider() -> Self {
        let mut v = Self::bare(UINodeType::Slider);
        v.interactive = true;
        v
    }

    /// A slider row — a typed building block. The host materialises a
    /// [`BitmapSlider`](crate::slider::BitmapSlider) into this node's laid rect
    /// at build, recording its ids under the node's [`key`](View::key) (so set
    /// one). Size it like any node (`.fill_w().h(Fixed(row_h))`); the spec is the
    /// build-time appearance, the live value rides on the panel's drag state.
    pub fn slider_row(spec: SliderSpec) -> Self {
        let mut v = Self::bare(UINodeType::Panel);
        v.slider = Some(Box::new(spec));
        v
    }

    /// A flexible empty space — fills its axis, eating leftover room so siblings
    /// pin to the edges. Transparent and non-interactive.
    pub fn spacer() -> Self {
        let mut v = Self::bare(UINodeType::Panel);
        v.width = Sizing::Fill;
        v.height = Sizing::Fill;
        v
    }

    // ── Sizing ──────────────────────────────────────────────────────

    pub fn w(mut self, s: Sizing) -> Self {
        self.width = s;
        self
    }

    pub fn h(mut self, s: Sizing) -> Self {
        self.height = s;
        self
    }

    /// Fixed width and height.
    pub fn fixed(mut self, w: f32, h: f32) -> Self {
        self.width = Sizing::Fixed(w);
        self.height = Sizing::Fixed(h);
        self
    }

    pub fn fill(mut self) -> Self {
        self.width = Sizing::Fill;
        self.height = Sizing::Fill;
        self
    }

    pub fn fill_w(mut self) -> Self {
        self.width = Sizing::Fill;
        self
    }

    pub fn fill_h(mut self) -> Self {
        self.height = Sizing::Fill;
        self
    }

    pub fn hug(mut self) -> Self {
        self.width = Sizing::Hug;
        self.height = Sizing::Hug;
        self
    }

    // ── Container config ────────────────────────────────────────────

    pub fn pad(mut self, pad: Pad) -> Self {
        self.pad = pad;
        self
    }

    pub fn gap(mut self, gap: f32) -> Self {
        self.gap = gap;
        self
    }

    pub fn main_align(mut self, a: Align) -> Self {
        self.main_align = a;
        self
    }

    pub fn cross_align(mut self, a: Align) -> Self {
        self.cross_align = a;
        self
    }

    pub fn child(mut self, child: View) -> Self {
        self.children.push(child);
        self
    }

    pub fn children(mut self, children: impl IntoIterator<Item = View>) -> Self {
        self.children.extend(children);
        self
    }

    // ── Style ───────────────────────────────────────────────────────

    /// Replace the whole style block (escape hatch for callers porting an
    /// existing `UIStyle`).
    pub fn style(mut self, style: UIStyle) -> Self {
        self.style = style;
        self
    }

    pub fn bg(mut self, c: Color32) -> Self {
        self.style.bg_color = c;
        self
    }

    pub fn hover_bg(mut self, c: Color32) -> Self {
        self.style.hover_bg_color = c;
        self
    }

    pub fn pressed_bg(mut self, c: Color32) -> Self {
        self.style.pressed_bg_color = c;
        self
    }

    pub fn border(mut self, color: Color32, width: f32) -> Self {
        self.style.border_color = color;
        self.style.border_width = width;
        self
    }

    pub fn radius(mut self, r: f32) -> Self {
        self.style.corner_radius = r;
        self
    }

    pub fn font(mut self, size: u16) -> Self {
        self.style.font_size = size;
        self
    }

    pub fn weight(mut self, w: FontWeight) -> Self {
        self.style.font_weight = w;
        self
    }

    pub fn text_color(mut self, c: Color32) -> Self {
        self.style.text_color = c;
        self
    }

    pub fn align_text(mut self, a: TextAlign) -> Self {
        self.style.text_align = a;
        self
    }

    // ── Flags ───────────────────────────────────────────────────────

    /// Clip children to this node's bounds (`CLIPS_CHILDREN`).
    pub fn clip(mut self) -> Self {
        self.clips = true;
        self
    }

    /// Force the `INTERACTIVE` flag (panels/labels are non-interactive by
    /// default; set this for a hit-catcher rect).
    pub fn interactive(mut self) -> Self {
        self.interactive = true;
        self
    }

    /// Mark an interactive node as deliberately intent-free (its gesture is
    /// handled in `handle_event`). Opts the node out of [`validate`].
    pub fn inert(mut self) -> Self {
        self.inert = true;
        self
    }

    /// Disable the node (`UIFlags::DISABLED`) — greyed, not hit-tested. A
    /// per-frame state, applied in place (toggling it is not a structural
    /// change), so a control can grey out without forcing a rebuild.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn visible(mut self, v: bool) -> Self {
        self.visible = v;
        self
    }

    /// Hide this node (still emitted, so toggling is an in-place update, not a
    /// structural change — the parent must reserve its space).
    pub fn hidden(mut self) -> Self {
        self.visible = false;
        self
    }

    pub fn key(mut self, k: u64) -> Self {
        self.key = Some(k);
        self
    }

    /// Pin this node's durable [`WidgetId`](crate::node::WidgetId) to a
    /// globally-derived identity (see the `identity` field doc). Card roots
    /// use `param_surface::stable_key(<card id>)`.
    pub fn identity(mut self, k: u64) -> Self {
        self.identity = Some(k);
        self
    }

    /// Register an automation component name for this node (`UI_AUTOMATION_DESIGN.md`
    /// D8/§3) — a static literal like `"transport.play"`. Applied once the view
    /// lands in the tree (`ChromeHost::build`/`materialize`); most views leave
    /// this unset.
    pub fn name(mut self, name: &'static str) -> Self {
        self.name = Some(name);
        self
    }

    // ── Intent ──────────────────────────────────────────────────────

    pub fn on_click(mut self, action: PanelAction) -> Self {
        self.intent.click = Some(action);
        self
    }

    pub fn on_double_click(mut self, action: PanelAction) -> Self {
        self.intent.double_click = Some(action);
        self
    }

    pub fn on_right_click(mut self, action: PanelAction) -> Self {
        self.intent.right_click = Some(action);
        self
    }

    /// Claim this node's whole area for intent fold-up — a gesture on any
    /// non-intent descendant resolves here.
    pub fn claims_area(mut self) -> Self {
        self.intent.claims_area = true;
        self
    }

    // ── Queries (used by layout/diff/validate) ──────────────────────

    pub(crate) fn has_intent(&self) -> bool {
        self.intent.is_set()
    }
}

/// Walk a view tree and collect validation warnings: every interactive node
/// (by kind or flag) that carries no intent and is not `.inert()` is a dead
/// control. Returns each warning as a human-readable path string.
pub fn validate(root: &View) -> Vec<String> {
    let mut out = Vec::new();
    validate_into(root, "root", &mut out);
    out
}

fn validate_into(view: &View, path: &str, out: &mut Vec<String>) {
    let is_interactive = view.interactive
        || matches!(
            view.kind,
            UINodeType::Button | UINodeType::Slider | UINodeType::Toggle
        );
    if is_interactive && !view.inert && !view.has_intent() {
        let label = view.text.as_deref().unwrap_or("<no text>");
        out.push(format!(
            "{path}: interactive {:?} \"{label}\" has no intent — call .on_click(...)/.on_right_click(...) or .inert()",
            view.kind
        ));
    }
    for (i, child) in view.children.iter().enumerate() {
        let child_path = format!("{path}>{i}");
        validate_into(child, &child_path, out);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let v = View::panel();
        assert_eq!(v.kind, UINodeType::Panel);
        assert_eq!(v.width, Sizing::Hug);
        assert!(v.visible);
        assert!(!v.interactive);
        assert!(!v.has_intent());
    }

    #[test]
    fn builders_compose() {
        let v = View::row(4.0)
            .pad(Pad::all(8.0))
            .child(View::label("hi").fill_w())
            .child(View::button("OK").fixed(40.0, 18.0).on_click(PanelAction::Transport(TransportAction::Stop)));
        assert_eq!(v.layout, Layout::Row);
        assert_eq!(v.gap, 4.0);
        assert_eq!(v.pad, Pad::all(8.0));
        assert_eq!(v.children.len(), 2);
        assert_eq!(v.children[0].width, Sizing::Fill);
        assert!(v.children[1].has_intent());
    }

    #[test]
    fn validate_flags_unwired_button() {
        let v = View::column(2.0)
            .child(View::button("dead")) // no intent, not inert
            .child(View::button("live").on_click(PanelAction::Transport(TransportAction::PlayPause)))
            .child(View::button("ok-inert").inert());
        let warnings = validate(&v);
        assert_eq!(warnings.len(), 1, "exactly the unwired button warns: {warnings:?}");
        assert!(warnings[0].contains("dead"));
    }

    #[test]
    fn validate_passes_claimed_area() {
        // A claims_area node counts as intent for validation (it absorbs).
        let v = View::panel().interactive().claims_area();
        assert!(validate(&v).is_empty());
    }

    #[test]
    fn spacer_fills_both_axes() {
        let s = View::spacer();
        assert_eq!(s.width, Sizing::Fill);
        assert_eq!(s.height, Sizing::Fill);
        assert!(!s.interactive);
    }
}
