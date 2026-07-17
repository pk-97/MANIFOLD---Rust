//! `AutomationAction` — the transport-agnostic core the agent drives MANIFOLD
//! through. `docs/UI_AUTOMATION_DESIGN.md` §3/§4: one `AutomationAction`
//! enum, resolved against the tree dump's data (the selector "DOM", §3) and
//! acted on by synthesizing input through the production path (D4). Both
//! transports (the headless `--script` runner, P2; the dev-only live door,
//! P3) reach the types and the resolver in this module — neither depends on
//! `manifold-app`, so this module stays `manifold-app`-free by construction.
//!
//! Gesture *synthesis* (turning a `Gesture` into real `UIRoot::pointer_event`
//! calls) is NOT here: `UIRoot` is a `manifold-app` type, and D4 says each
//! mode injects at its own proven seam. This module only resolves *targets*
//! to rects — the mechanical, transport-agnostic half of "find, act, wait".

use crate::hit_targets::HitTargets;
use crate::input::{Key, Modifiers};
use crate::node::{Rect, Vec2};
use crate::tree::UITree;

// ── §4 committed action model ───────────────────────────────────────────

/// One automation request. Transport-agnostic: the ui-snap script driver
/// (headless) and the dev TCP server (live, P3) both compile scripts down to
/// this. `UI_AUTOMATION_DESIGN.md` §4 — committed shape, transcribed exactly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AutomationAction {
    /// Resolve `target` against the current build, synthesize the gesture
    /// through the production input path (D4).
    Pointer {
        target: AutomationTarget,
        gesture: Gesture,
    },
    Key {
        key: Key,
        modifiers: Modifiers,
    },
    /// Text through the real TextInput path (focused field).
    Text {
        text: String,
    },
    /// Advance the deterministic clock by `frames` at fixed `dt` (headless);
    /// in live mode, wait `frames` real frames.
    Step {
        frames: u32,
    },
    /// Emit the extended dump (§3) to the run's output dir / reply.
    Dump,
    /// Emit a PNG of the current UI to the run's output dir / reply.
    Snapshot,
    /// D10 assertion; failure = loud stop with dump attached.
    Assert {
        selector: AutomationTarget,
        check: AssertCheck,
    },
}

/// §4 committed shape. `Surface`'s `surface` field is `&'static str` in the
/// design doc (matching `HitTargets::surface_id()`'s return type); a JSON
/// script can't hand us a `'static` borrow, so the `Deserialize` impl leaks
/// the parsed string once via `leak_surface_id` — cheap and correct for a
/// one-shot script process, and it keeps the field's *type* exactly what the
/// doc committed (so in-process construction, e.g. from the live door in P3,
/// still just writes a literal).
#[derive(Debug, Clone, serde::Serialize)]
pub enum AutomationTarget {
    /// §3 structural query.
    Query(SelectorQuery),
    /// A `WidgetId` raw value from a prior dump.
    Widget(u64),
    /// §5 custom-surface target.
    Surface {
        surface: &'static str,
        kind: String,
        label: String,
    },
    /// Escape hatch — D2 restrictions apply (a script using `Point` where a
    /// widget/surface target exists fails review; not runtime-enforced).
    Point(Vec2),
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Gesture {
    Click { modifiers: Modifiers },
    DoubleClick,
    /// The other house intrinsic-reset gesture (BUG-070/BUG-105's "every
    /// card/panel slider in the app" convention) — a right-click at the
    /// target's centre. Mirrors `Click`'s single down/up, routed through
    /// `UIInputSystem::process_right_click` (the same call
    /// `window_input.rs`'s real right-button handler makes) instead of a
    /// plain pointer down/up, so it always emits `UIEvent::RightClick` —
    /// including a miss, which still carries `pos` for position-based
    /// consumers.
    RightClick,
    Hover,
    /// Down at target, interpolated Move steps (real drag thresholds must
    /// fire), Up at `to`. `steps` ≥ 2.
    Drag { to: AutomationTarget, steps: u32 },
    Scroll { delta: Vec2 },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum AssertCheck {
    Exists,
    TextEquals(String),
    Count(u32),
    RectWithin(Rect),
}

/// §3 selector — a structural query over the dump. Resolution: filter nodes
/// by `name`/`text`/`type`; `under_text` walks ancestors until a node whose
/// `text` matches; `nth` disambiguates; exactly-one match required.
///
/// `#[serde(default)]`: a script only names the fields it filters on (§3's
/// own worked examples are single- and double-field objects); every omitted
/// field defaults to `None` rather than being a parse error.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct SelectorQuery {
    pub name: Option<String>,
    pub text: Option<String>,
    #[serde(rename = "type")]
    pub node_type: Option<String>,
    pub under_text: Option<String>,
    pub nth: Option<usize>,
}

// Manual `Deserialize` for `AutomationTarget` so the `Surface.surface` field
// keeps the doc's committed `&'static str` type (see the doc comment above)
// while still round-tripping through JSON.
impl<'de> serde::Deserialize<'de> for AutomationTarget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(serde::Deserialize)]
        enum Raw {
            Query(SelectorQuery),
            Widget(u64),
            Surface {
                surface: String,
                kind: String,
                label: String,
            },
            Point(Vec2),
        }
        Ok(match Raw::deserialize(deserializer)? {
            Raw::Query(q) => AutomationTarget::Query(q),
            Raw::Widget(w) => AutomationTarget::Widget(w),
            Raw::Surface { surface, kind, label } => AutomationTarget::Surface {
                surface: leak_surface_id(surface),
                kind,
                label,
            },
            Raw::Point(p) => AutomationTarget::Point(p),
        })
    }
}

/// Leak a parsed surface id to `'static`. One-shot script processes only —
/// never called on a hot path, and the set of distinct surface ids a script
/// can name is bounded by the JSON file's own size.
fn leak_surface_id(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

// ── Resolver (§3) ────────────────────────────────────────────────────────

/// A target resolved to the rect a gesture should act on, plus a human
/// description (evidence for `result.json` / failure messages).
#[derive(Debug, Clone)]
pub struct ResolvedTarget {
    pub rect: Rect,
    pub description: String,
}

/// Why a target failed to resolve — D6: zero or >1 match is a hard failure
/// that lists the candidates.
#[derive(Debug, Clone)]
pub enum ResolveError {
    NoMatch { query: String },
    Ambiguous { query: String, candidates: Vec<String> },
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NoMatch { query } => write!(f, "no match for {query}"),
            ResolveError::Ambiguous { query, candidates } => write!(
                f,
                "{} matches for {query} (need exactly 1): {}",
                candidates.len(),
                candidates.join(", ")
            ),
        }
    }
}

/// One match against a target — the common currency `resolve` (exactly-one)
/// and the `Assert` evaluator (which needs to see zero/many matches for
/// `Count`, not just a single winner) both build on.
#[derive(Debug, Clone)]
pub struct MatchInfo {
    pub rect: Rect,
    /// `None` for targets with no natural text (a raw `Point`, most custom
    /// surfaces) — `Query` matches always carry the node's own `text`.
    pub text: Option<String>,
    pub description: String,
}

/// Every match for `target` against the current build, infallible — used by
/// `resolve` (which then enforces exactly-one, D6) and by `Assert`'s `Count`
/// check (which needs the raw count, not a single winner).
pub fn resolve_all(
    tree: &UITree,
    surfaces: &[&dyn HitTargets],
    target: &AutomationTarget,
) -> (Vec<MatchInfo>, String) {
    match target {
        AutomationTarget::Point(p) => (
            vec![MatchInfo {
                rect: Rect::new(p.x, p.y, 0.0, 0.0),
                text: None,
                description: format!("point ({:.1},{:.1})", p.x, p.y),
            }],
            format!("point({:.1},{:.1})", p.x, p.y),
        ),
        AutomationTarget::Widget(raw) => all_widget_matches(tree, *raw),
        AutomationTarget::Surface { surface, kind, label } => {
            all_surface_matches(surfaces, surface, kind, label)
        }
        AutomationTarget::Query(q) => all_query_matches(tree, q),
    }
}

/// Resolve `target` against the current build (`tree` + the enumerated
/// custom `surfaces`, §5) to exactly one rect. D6: zero or >1 candidate is a
/// hard failure carrying the candidate list as evidence.
pub fn resolve(
    tree: &UITree,
    surfaces: &[&dyn HitTargets],
    target: &AutomationTarget,
) -> Result<ResolvedTarget, ResolveError> {
    let (matches, query) = resolve_all(tree, surfaces, target);
    match matches.len() {
        0 => Err(ResolveError::NoMatch { query }),
        1 => {
            let m = matches.into_iter().next().expect("len checked above");
            Ok(ResolvedTarget { rect: m.rect, description: m.description })
        }
        _ => Err(ResolveError::Ambiguous {
            query,
            candidates: matches.iter().map(|m| m.description.clone()).collect(),
        }),
    }
}

fn all_widget_matches(tree: &UITree, raw: u64) -> (Vec<MatchInfo>, String) {
    let nodes = tree.nodes();
    let query = format!("widget({raw:016x})");
    let matches = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.flags.contains(crate::node::UIFlags::INTERACTIVE) && tree.widget_of(n.id).raw() == raw)
        .map(|(i, _)| node_match_info(tree, nodes, i))
        .collect();
    (matches, query)
}

fn all_surface_matches(
    surfaces: &[&dyn HitTargets],
    surface: &str,
    kind: &str,
    label: &str,
) -> (Vec<MatchInfo>, String) {
    let query = format!("surface{{surface:{surface:?}, kind:{kind:?}, label:{label:?}}}");
    let mut entries = Vec::new();
    for s in surfaces {
        if s.surface_id() == surface {
            s.enumerate(&mut entries);
        }
    }
    let matches = entries
        .iter()
        .filter(|e| e.kind == kind && e.label == label)
        .map(|e| MatchInfo {
            rect: e.rect,
            text: Some(e.label.clone()),
            description: format!("{} '{}' ({})", e.kind, e.label, e.payload),
        })
        .collect();
    (matches, query)
}

fn all_query_matches(tree: &UITree, q: &SelectorQuery) -> (Vec<MatchInfo>, String) {
    let nodes = tree.nodes();
    let hits: Vec<usize> = (0..nodes.len()).filter(|&i| node_matches(tree, nodes, i, q)).collect();
    let query = describe_query(q);

    if let Some(nth) = q.nth {
        return match hits.get(nth).copied() {
            Some(i) => (vec![node_match_info(tree, nodes, i)], format!("{query} [nth={nth}]")),
            None => (
                Vec::new(),
                format!("{query} [nth={nth} of {} candidates]", hits.len()),
            ),
        };
    }

    let matches = hits.iter().map(|&i| node_match_info(tree, nodes, i)).collect();
    (matches, query)
}

fn node_match_info(tree: &UITree, nodes: &[crate::node::UINode], i: usize) -> MatchInfo {
    MatchInfo {
        rect: nodes[i].bounds,
        text: nodes[i].text.clone(),
        description: describe_node(tree, nodes, i),
    }
}

fn node_matches(
    tree: &UITree,
    nodes: &[crate::node::UINode],
    i: usize,
    q: &SelectorQuery,
) -> bool {
    let n = &nodes[i];
    if let Some(name) = &q.name
        && tree.name_of(n.id) != Some(name.as_str())
    {
        return false;
    }
    if let Some(text) = &q.text
        && n.text.as_deref() != Some(text.as_str())
    {
        return false;
    }
    if let Some(t) = &q.node_type
        && &format!("{:?}", n.node_type) != t
    {
        return false;
    }
    if let Some(under) = &q.under_text
        && !under_text_matches(nodes, i, under)
    {
        return false;
    }
    true
}

/// §3: "under_text walks ancestors until a node whose text matches (how 'the
/// mute button of the PLASMA row' works without per-row name allocation)".
///
/// Real row chrome (`layer_header.rs`) doesn't put the row's name text on an
/// ancestor of its child controls — the name button and the mute button are
/// SIBLINGS under a shared, unlabeled row container (verified against a live
/// dump: node `111` — the row — has `text: None`; `115` "PLASMA" and `121`
/// "layer_header.mute" are both direct children of `111`). A literal
/// ancestor-chain walk from the mute button would never see "PLASMA", so it
/// can't be what makes the doc's own worked example resolve. The semantics
/// that DOES match "the mute button of the PLASMA row": `nodes[i]` is
/// "under" a text match if they share a common ancestor — i.e. the text-match
/// node is either a literal ancestor of `nodes[i]` (the doc's literal case)
/// or a descendant of one of `nodes[i]`'s ancestors (the sibling-row case
/// that's actually shipped). Both are "the same row" to a human reading the
/// dump; this is the resolver matching that intent instead of a topology
/// the real tree doesn't have.
///
/// BUG-192: the ORIGINAL version of this function (git history) walked from
/// EVERY node whose text equalled `under`, all the way up to the tree root,
/// and returned `true` the instant that walk crossed ANY ancestor `nodes[i]`
/// also has — not the NEAREST one. Two failure modes fell out of that:
///
/// 1. Zero match: `param_card.rs`'s generator rows parent every row's
///    label/track/value_text FLAT to the literal tree root
///    (`build_generator`/`build_param_row`'s `parent: None` — "generators
///    parent rows flat to the root", source comment) — label, slider, and
///    value across the WHOLE card share `parent_id: None`. A `None`-parented
///    node's ancestor chain is empty, so neither check above could ever
///    fire — a flat-sibling row's `under_text` query always returned zero
///    matches, full stop, no matter which row.
/// 2. Cross-match: a REAL dock with per-row containers nested under one
///    shared OUTER container (`layer_header.rs`'s actual shape — every row's
///    `row_clip` is itself a child of ONE shared scroll `clip_parent`) could
///    walk THROUGH that shared outer container and match a DIFFERENT row's
///    label, because "any shared ancestor, however far up" is true for
///    almost every two nodes in a real tree (they all share the scroll
///    clip, the dock, eventually the UI root). `under_text_walks_ancestors`
///    (below) never caught this — its two rows are each parented straight
///    to `None`, with no outer container in common.
///
/// The fix: walk OUTWARD from `nodes[i]` one enclosing level at a time
/// (`level`, `level`'s parent, that parent's parent, …). At each level,
/// first check whether the level's own immediate parent literally carries
/// `under` as its text (the doc's literal-ancestor case). Otherwise, scan
/// backward through nodes sharing that SAME parent_id (real container or
/// `None`) for the NEAREST preceding one with ANY text — build order puts a
/// row's own label before the rest of that row's controls, so this is "the
/// nearest labeled row" without needing a real container to define "row" at
/// all. Stopping at the first ANY-text node (rather than scanning past it)
/// is what keeps two same-level rows from cross-matching; only climbing to
/// the next level when a level has NO texted sibling at all (never found
/// any signal there) is what keeps the walk from tunnelling through a
/// shared outer container the way the original all-the-way-to-root walk
/// did. Terminates: a parent's index is always lower than its child's (a
/// node can only reference a `NodeId` that already exists), so `level`
/// strictly decreases every climb.
fn under_text_matches(nodes: &[crate::node::UINode], i: usize, under: &str) -> bool {
    let mut level = i;
    loop {
        let parent = nodes[level].parent_id;
        // Literal-ancestor case: the enclosing container itself carries
        // `under` as its own text.
        if let Some(p) = parent
            && nodes[p.index()].text.as_deref() == Some(under)
        {
            return true;
        }
        // Nearest-labeled-row case: the closest preceding same-parent
        // sibling that carries ANY text decides this level, match or not.
        if let Some(found) =
            (0..level).rev().find(|&j| nodes[j].parent_id == parent && nodes[j].text.is_some())
        {
            return nodes[found].text.as_deref() == Some(under);
        }
        match parent {
            Some(p) => level = p.index(),
            None => return false,
        }
    }
}

fn describe_query(q: &SelectorQuery) -> String {
    let mut parts = Vec::new();
    if let Some(v) = &q.name {
        parts.push(format!("name={v:?}"));
    }
    if let Some(v) = &q.text {
        parts.push(format!("text={v:?}"));
    }
    if let Some(v) = &q.node_type {
        parts.push(format!("type={v:?}"));
    }
    if let Some(v) = &q.under_text {
        parts.push(format!("under_text={v:?}"));
    }
    format!("query{{{}}}", parts.join(", "))
}

fn describe_node(tree: &UITree, nodes: &[crate::node::UINode], i: usize) -> String {
    let n = &nodes[i];
    format!(
        "#{} {:?} name={:?} text={:?} rect=({:.0},{:.0} {:.0}x{:.0})",
        n.id.index(),
        n.node_type,
        tree.name_of(n.id),
        n.text,
        n.bounds.x,
        n.bounds.y,
        n.bounds.width,
        n.bounds.height
    )
}


// ── Gesture math (transport-agnostic; the synthesis of real events from
// this math is `manifold-app`'s job, D4) ─────────────────────────────────

/// Interpolated points for a `Drag { to, steps }` gesture: `steps` evenly
/// spaced points strictly between `from` and `to` (excludes both endpoints —
/// the caller synthesizes Down at `from` and Up at `to` separately), so the
/// caller emits `steps` `Move` events between them. `steps < 2` is clamped to
/// 2 (§4: "`steps` ≥ 2" — a single Move can't be guaranteed to cross the
/// drag threshold AND still leave room for a subsequent Move to register the
/// real per-frame drag update).
pub fn interpolate_drag(from: Vec2, to: Vec2, steps: u32) -> Vec<Vec2> {
    let steps = steps.max(2);
    (1..=steps)
        .map(|i| {
            let t = i as f32 / (steps + 1) as f32;
            Vec2::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::{UIFlags, UINodeType, UIStyle};
    use crate::tree::UITree;

    fn button(tree: &mut UITree, text: &str) -> crate::node::NodeId {
        tree.add_button(None, 0.0, 0.0, 10.0, 10.0, UIStyle::default(), text)
    }

    #[test]
    fn exactly_one_match_resolves() {
        let mut tree = UITree::new();
        button(&mut tree, "Bloom");
        button(&mut tree, "Mirror");
        let q = SelectorQuery { text: Some("Bloom".into()), ..Default::default() };
        let resolved = resolve(&tree, &[], &AutomationTarget::Query(q)).expect("resolves");
        assert_eq!(resolved.rect.width, 10.0);
    }

    #[test]
    fn zero_match_fails() {
        let mut tree = UITree::new();
        button(&mut tree, "Bloom");
        let q = SelectorQuery { text: Some("Glow".into()), ..Default::default() };
        let err = resolve(&tree, &[], &AutomationTarget::Query(q)).unwrap_err();
        assert!(matches!(err, ResolveError::NoMatch { .. }));
    }

    #[test]
    fn multi_match_fails_with_candidates() {
        let mut tree = UITree::new();
        button(&mut tree, "Bloom");
        button(&mut tree, "Bloom");
        let q = SelectorQuery { text: Some("Bloom".into()), ..Default::default() };
        let err = resolve(&tree, &[], &AutomationTarget::Query(q)).unwrap_err();
        match err {
            ResolveError::Ambiguous { candidates, .. } => assert_eq!(candidates.len(), 2),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn nth_disambiguates() {
        let mut tree = UITree::new();
        button(&mut tree, "Row");
        let second = button(&mut tree, "Row");
        let q = SelectorQuery { text: Some("Row".into()), nth: Some(1), ..Default::default() };
        let resolved = resolve(&tree, &[], &AutomationTarget::Query(q)).expect("resolves");
        let expected = tree.nodes()[second.index()].bounds;
        assert_eq!(resolved.rect, expected);
    }

    #[test]
    fn under_text_walks_ancestors() {
        let mut tree = UITree::new();
        let row_a = tree.add_panel(None, 0.0, 0.0, 100.0, 20.0, UIStyle::default());
        let row_a_label = tree.add_node(
            Some(row_a),
            crate::node::Rect::new(0.0, 0.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("PLASMA"),
            UIFlags::empty(),
        );
        let _ = row_a_label;
        let mute_a = tree.add_button(Some(row_a), 40.0, 0.0, 10.0, 10.0, UIStyle::default(), "mute");

        let row_b = tree.add_panel(None, 0.0, 20.0, 100.0, 20.0, UIStyle::default());
        tree.add_node(
            Some(row_b),
            crate::node::Rect::new(0.0, 20.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("FLOWERS"),
            UIFlags::empty(),
        );
        tree.add_button(Some(row_b), 40.0, 20.0, 10.0, 10.0, UIStyle::default(), "mute");

        let q = SelectorQuery {
            text: Some("mute".into()),
            under_text: Some("PLASMA".into()),
            ..Default::default()
        };
        let resolved = resolve(&tree, &[], &AutomationTarget::Query(q)).expect("resolves");
        assert_eq!(resolved.rect, tree.nodes()[mute_a.index()].bounds);
    }

    /// BUG-192: `under_text_walks_ancestors` (above) only covers the
    /// "tight container" shape — each row gets its OWN real per-row parent
    /// `NodeId`, so a shared non-`None` ancestor always exists between a
    /// row's label and its controls. `param_card.rs`'s generator rows don't
    /// build that way: `build_generator`/`build_param_row` parent every
    /// row's label/track/value_text FLAT to the literal tree root (`parent:
    /// None` — "generators parent rows flat to the root" per the source
    /// comment), so label, slider, and value across EVERY row in the whole
    /// card share the exact same (`None`) parent_id. `candidate_ancestors`
    /// is empty for a `None`-parented node, so neither the literal-ancestor
    /// nor the common-ancestor check in `under_text_matches` can ever fire
    /// — a flat-sibling row's `under_text` query always returned zero
    /// matches, regardless of which row was queried. Node order mirrors
    /// `slider.rs::Slider::build`'s real emission (label, then the
    /// draggable track, THEN the value readout last) — the value node
    /// trails the control a script actually targets, so it can never sit
    /// between a row's label and that control.
    #[test]
    fn under_text_resolves_flat_sibling_rows() {
        let mut tree = UITree::new();
        tree.add_node(
            None,
            crate::node::Rect::new(0.0, 0.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("Density"),
            UIFlags::VISIBLE | UIFlags::INTERACTIVE,
        );
        let slider_a = tree.add_button(None, 40.0, 0.0, 40.0, 20.0, UIStyle::default(), "S1");
        tree.add_node(
            None,
            crate::node::Rect::new(80.0, 0.0, 20.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("0.75"),
            UIFlags::empty(),
        );

        tree.add_node(
            None,
            crate::node::Rect::new(0.0, 20.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("Speed"),
            UIFlags::VISIBLE | UIFlags::INTERACTIVE,
        );
        tree.add_button(None, 40.0, 20.0, 40.0, 20.0, UIStyle::default(), "S2");
        tree.add_node(
            None,
            crate::node::Rect::new(80.0, 20.0, 20.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("0.30"),
            UIFlags::empty(),
        );

        let q = SelectorQuery {
            text: Some("S1".into()),
            under_text: Some("Density".into()),
            ..Default::default()
        };
        let resolved = resolve(&tree, &[], &AutomationTarget::Query(q)).expect("resolves");
        assert_eq!(resolved.rect, tree.nodes()[slider_a.index()].bounds);

        // Cross-row: row 2's slider must NOT resolve under row 1's label —
        // "stop at the first node carrying ANY text" is what keeps two
        // flat, same-parent rows from cross-matching each other.
        let q2 = SelectorQuery {
            text: Some("S2".into()),
            under_text: Some("Density".into()),
            ..Default::default()
        };
        assert!(resolve(&tree, &[], &AutomationTarget::Query(q2)).is_err());
    }

    /// BUG-192 regression guard, `layer_header.rs`'s real shape: EVERY
    /// layer row's per-row `row_clip` is itself parented to ONE shared
    /// scroll `clip_parent` (`build_layer_row`'s `clip_parent` param) — an
    /// extra shared level `under_text_walks_ancestors` never exercised
    /// (that test's rows are each parented straight to `None`, with no
    /// outer container in common). A common-ancestor walk that climbs
    /// unconditionally could cross THROUGH that shared scroll clip and
    /// match a completely different row's label — this pins that it
    /// doesn't: the mute button resolves to its OWN row only.
    #[test]
    fn under_text_layer_header_shared_scroll_clip_does_not_cross_match() {
        let mut tree = UITree::new();
        let clip_parent = tree.add_panel(None, 0.0, 0.0, 100.0, 40.0, UIStyle::default());

        let row_a_clip = tree.add_panel(Some(clip_parent), 0.0, 0.0, 100.0, 20.0, UIStyle::default());
        tree.add_node(
            Some(row_a_clip),
            crate::node::Rect::new(0.0, 0.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("PLASMA"),
            UIFlags::empty(),
        );
        let mute_a = tree.add_button(Some(row_a_clip), 40.0, 0.0, 10.0, 10.0, UIStyle::default(), "mute");

        let row_b_clip = tree.add_panel(Some(clip_parent), 0.0, 20.0, 100.0, 20.0, UIStyle::default());
        tree.add_node(
            Some(row_b_clip),
            crate::node::Rect::new(0.0, 20.0, 40.0, 20.0),
            UINodeType::Label,
            UIStyle::default(),
            Some("FLOWERS"),
            UIFlags::empty(),
        );
        tree.add_button(Some(row_b_clip), 40.0, 20.0, 10.0, 10.0, UIStyle::default(), "mute");

        let q = SelectorQuery {
            text: Some("mute".into()),
            under_text: Some("PLASMA".into()),
            ..Default::default()
        };
        let resolved = resolve(&tree, &[], &AutomationTarget::Query(q)).expect("resolves");
        assert_eq!(resolved.rect, tree.nodes()[mute_a.index()].bounds);
    }

    #[test]
    fn point_target_resolves_directly() {
        let tree = UITree::new();
        let resolved = resolve(&tree, &[], &AutomationTarget::Point(Vec2::new(5.0, 6.0))).unwrap();
        assert_eq!(resolved.rect.x, 5.0);
        assert_eq!(resolved.rect.y, 6.0);
    }

    #[test]
    fn interpolate_drag_produces_at_least_steps_moves() {
        let moves = interpolate_drag(Vec2::new(0.0, 0.0), Vec2::new(100.0, 0.0), 4);
        assert_eq!(moves.len(), 4);
        // Strictly increasing x, strictly between the endpoints — a real
        // per-frame drag path, not a Down/Up teleport.
        assert!(moves[0].x > 0.0 && moves[0].x < moves[1].x);
        assert!(moves.last().unwrap().x < 100.0);
    }

    #[test]
    fn interpolate_drag_clamps_steps_below_two() {
        assert_eq!(interpolate_drag(Vec2::ZERO, Vec2::new(10.0, 0.0), 0).len(), 2);
        assert_eq!(interpolate_drag(Vec2::ZERO, Vec2::new(10.0, 0.0), 1).len(), 2);
    }

    #[test]
    fn serde_round_trips_through_json() {
        let action = AutomationAction::Pointer {
            target: AutomationTarget::Surface {
                surface: "timeline_clips",
                kind: "clip".into(),
                label: "flowers_loop_B.mov".into(),
            },
            gesture: Gesture::Drag { to: AutomationTarget::Point(Vec2::new(10.0, 20.0)), steps: 6 },
        };
        let json = serde_json::to_string(&action).expect("serialize");
        let back: AutomationAction = serde_json::from_str(&json).expect("deserialize");
        match back {
            AutomationAction::Pointer {
                target: AutomationTarget::Surface { surface, kind, label },
                gesture: Gesture::Drag { steps, .. },
            } => {
                assert_eq!(surface, "timeline_clips");
                assert_eq!(kind, "clip");
                assert_eq!(label, "flowers_loop_B.mov");
                assert_eq!(steps, 6);
            }
            other => panic!("unexpected round-trip: {other:?}"),
        }
    }
}
