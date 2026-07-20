# Overlay Sessions & Picker Core — per-open state that cannot go stale, and one reusable pick-from-a-list component

**Status:** SHIPPED — P1–P2 landed 2026-07-04/05 (`PickerCore`, `BrowserSession`, owned text sessions all live in-tree) · designed 2026-07-04 · Fable
**Prerequisites:** none (extends the SHIPPED overlay driver, `docs/OVERLAY_SYSTEM_DESIGN.md`)
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 before starting any phase.

The governing insight: the overlay driver (shipped 2026-06-16) unified *hosting* —
build, draw, input routing, z-order — but left each overlay's *contents and state
lifecycle* bespoke. Every popup hand-clears its own fields on open/close, and the
global text-input session lives on `App`, outside any overlay's reach. Both are the
same bug class: **per-open state whose reset is distributed across close paths.**
Peter hit it live in the browser popup ("the search and text seems to stay after you
search and need to click elsewhere again to close it properly") and set the bar:
**"We should fix these types of bugs at the fundamental level so they no longer
happen with other GUI features too"** and **"surely a better more fundamental API
and 'bundle' that's reused for these types of things?"** (2026-07-04). The fix is to
make stale state unrepresentable — per-open state is *constructed at open and
dropped at close*, never reset field-by-field — and to extract the one component
three surfaces already reimplement: search field + category chips + filtered list +
keyboard nav + pick-one-and-close.

Companion docs: `OVERLAY_SYSTEM_DESIGN.md` (the shipped driver this extends — read
its "Refinements" + "Status" sections, the as-built diverges from its own design
body) · `PRESET_LIBRARY_DESIGN.md` (the library browser is this design's second
consumer; its P3 has a hard edge on this doc's P2).

## 1. Audit — what exists (verified 2026-07-04)

| Piece | Where | State |
|---|---|---|
| Overlay trait + driver | `crates/manifold-ui/src/panels/overlay.rs` (trait, `Modality`, `Anchor`); driver on `UIRoot` (`overlay_mut`, `build_overlays`, `route_overlay_event`, `escape_overlays`) | SHIPPED. Overlays self-close by flipping their own open flag; the driver only reads `is_open()` — there is no close hook and no per-open state ownership. |
| Browser popup | `crates/manifold-ui/src/panels/browser_popup.rs` | One long-lived struct reused across opens. `open()` hand-clears 8 fields (line 236), `close()` hand-clears 9 (line 255). Serves THREE modes already: Effect, Generator, Node (add-node picker in the graph editor) — `BrowserPopupMode`, line 64. Search + category chips + grid + paste button. |
| Search text session | `crates/manifold-app/src/text_input.rs` — `TextInputState` (line 155), one global single-session manager on `App`; `TextInputField::SearchFilter` (line 49) | The popup's search text lives in TWO places: the app-global text session (`self.text_input`) and `browser_popup.current_filter` (line 145), synced by commit/keystroke events (`window_input.rs:1605`). Nothing ties the session's lifetime to the popup's: `browser_popup.close()` cannot reach `self.text_input` (crate boundary), so a closed popup can leave an orphaned SearchFilter session — Peter's reported symptom. |
| SearchFilter begin sites | `app_render.rs:918` (search-bar click, editor window), `app_render.rs:1461-1472` (`BrowserSearchClicked`, main window), `app_render.rs:2007` (auto-focus at node-picker open) | Three hand-wired call sites across two windows; none is paired with a guaranteed end-on-popup-close. |
| Stale-context precedent inside TextInputState | `text_input.rs:226-229` and `cancel()` line 233 | The struct itself carries five per-session `Option` context fields with comments warning about stale leaks — the same distributed-reset pattern one level down. |
| Dropdown | `crates/manifold-ui/src/panels/dropdown.rs` | Modeless picker, first-char jump nav (line 632), no search field. Own state lifecycle. |
| Ableton picker | `crates/manifold-ui/src/panels/ableton_picker.rs` | Modal picker, no search. Own state lifecycle. |
| Browser item plumbing | `ui_root.rs:1420-1490` (Effect/Generator open sites build parallel `Vec<String>`s), `app_render.rs:1950-1998` (Node mode: labels + type ids + categories + alias search text) | Items travel as 4–5 parallel `Vec`s in `BrowserPopupRequest` — no item struct. |

**Extend, don't redesign.** The overlay driver's shape (typed fields on `UIRoot`,
`OverlayId` exhaustive dispatch, stash-and-drain lowering) is settled and stays.

## 2. Decisions

- **D1 — Per-open state is a session struct, dropped on close.** Every overlay with
  open-time state splits into long-lived config (ids, screen size, caches) and a
  `session: Option<SessionT>`; `is_open()` becomes `session.is_some()`; `open(req)`
  constructs the session whole; close is `self.session = None`. No field-by-field
  reset exists to forget. *Rejected:* auditing and completing the existing
  `open()`/`close()` clear-lists, because that fixes instances, not the class — the
  next field added recreates the bug (this is `eliminate-bug-class-at-storage-layer`
  applied to UI state).
- **D2 — Text-input sessions carry an owner; the app cancels them when the owner
  closes.** `TextInputState` gains `owner: Option<TextSessionOwner>`; overlays that
  host a text field are opened through helpers that tag the session; the app-side
  overlay pump cancels any session whose owner just closed. *Rejected:* moving the
  text session into `manifold-ui` so the popup can own it — the session coordinates
  keyboard interception and the field-overlay render at the app layer across two
  windows; dragging it down repeats the layering violation the overlay design
  already rejected for `dropdown_context` (OVERLAY_SYSTEM_DESIGN "Refinements").
- **D3 — One `PickerCore` component owns the pick-from-a-list model.** Items,
  categories, filter, filtered indices, hover/keyboard cursor, scroll — plus the
  interaction rules (typing filters, chips filter, arrows move, Enter picks,
  Escape dismisses). Rendering stays per-surface (the browser draws grid cells;
  a future list-style picker draws rows). *Rejected:* a full widget that also
  renders, because the browser grid and dropdown rows share no drawing and forcing
  one draw path would be a rewrite of working pixels for zero behavior gain.
- **D4 — Scope fence: only the browser popup migrates in this design.** Dropdown
  and Ableton picker keep their current internals; their state is small and not
  currently buggy. They adopt D1/D3 only when next touched for a real reason
  (see Deferred). Peter asked for the reusable bundle, not a popup rewrite spree —
  the bundle is proven by its two real consumers: this migration and the library
  browser (PRESET_LIBRARY P3).
- **D5 — `PickerItem` replaces the parallel `Vec`s** in `BrowserPopupRequest`. The
  three open sites construct `Vec<PickerItem>` instead of 4–5 aligned vectors.

## 3. Design — the session contract

New module `crates/manifold-ui/src/panels/overlay_session.rs` is documentation +
one helper trait; the contract is a convention the compiler enforces via shape,
not a framework:

```rust
/// Convention (documented on the Overlay trait): an overlay with per-open state
/// holds it as `session: Option<S>`. `is_open()` == `self.session.is_some()`.
/// Opening constructs S whole; closing assigns None. An overlay MUST NOT carry
/// a mutable field that is meaningful only while open outside its session type.
```

`BrowserPopup` becomes:

```rust
pub struct BrowserPopup {
    // config (survives across opens)
    screen_w: f32,
    screen_h: f32,
    session: Option<BrowserSession>,
}

pub struct BrowserSession {
    pub mode: BrowserPopupMode,
    pub tab: InspectorTab,
    pub layer_id: Option<LayerId>,
    pub picker: PickerCore,          // items, filter, chips, nav, scroll — §4
    pub pending_spawn_graph_pos: Option<(f32, f32)>,
    pub paste_count: usize,
    // layout output (rects, cell/chip NodeIds) — rebuilt every build_at
    layout: BrowserLayout,
}
```

`close()` is `self.session = None;` — one line, nothing to forget.

### Text-session ownership (D2)

```rust
// crates/manifold-app/src/text_input.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextSessionOwner {
    MainOverlay(manifold_ui::panels::OverlayId),
    EditorOverlay(manifold_ui::panels::OverlayId), // graph-editor window's UIRoot
}

pub struct TextInputState {
    pub owner: Option<TextSessionOwner>,   // NEW; None = panel-owned (BPM field etc.)
    // ... existing fields unchanged
}

impl TextInputState {
    /// begin() + owner tag, for fields hosted inside an overlay.
    pub fn begin_owned(&mut self, owner: TextSessionOwner, field: TextInputField,
                       initial: &str, anchor: AnchorRect, font_size: f32);
    /// Cancel iff the active session is owned by `owner`. Called by the app's
    /// overlay pump when an overlay flips closed.
    pub fn cancel_if_owned_by(&mut self, owner: TextSessionOwner);
}
```

The driver side: `UIRoot` records overlays whose `is_open()` flipped false during
event routing / escape (`closed_overlays: SmallVec<[OverlayId; 2]>`, drained by the
app via `take_closed_overlays()` — the same stash-and-drain shape as
`drain_overlay_selections`). The app pump, once per frame per window, maps each
drained id to a `TextSessionOwner` and calls `cancel_if_owned_by`. **This closes
Peter's bug for every current and future overlay-hosted text field, not just the
browser search.** The three raw `begin(SearchFilter, ...)` sites become
`begin_owned(...)`; a raw `begin` with `SearchFilter` becomes a clippy-visible
anachronism (negative gate below).

The plausible-wrong architecture, forbidden by name: **you will want the popup's
`close()` to clear the text session directly, or to give the `Overlay` trait a
`close()` hook that does it.** No — `manifold-ui` cannot see `TextInputState`
(layering), and the shipped driver deliberately has no close hook (overlays
self-close). The stash-and-drain pump is the house pattern; use it.

## 4. Design — PickerCore

New file `crates/manifold-ui/src/panels/picker_core.rs`:

```rust
pub struct PickerItem {
    pub label: String,
    pub type_id: String,
    pub category: Option<String>,
    /// Extra haystack (aliases etc.); filter matches label + this.
    pub search_text: Option<String>,
    /// Origin badge for library surfaces (PRESET_LIBRARY): e.g. User / Project.
    pub badge: Option<String>,
}

pub struct PickerCore {
    items: Vec<PickerItem>,
    categories: Vec<String>,
    active_category: Option<String>,
    filter: String,
    filtered: Vec<usize>,        // indices into items
    cursor: Option<usize>,       // keyboard position within filtered
    pub scroll: ScrollContainer, // reuse the existing scroll widget
}

pub enum PickerNav { Moved, Picked(usize /* items index */), Dismissed, Ignored }

impl PickerCore {
    pub fn new(items: Vec<PickerItem>, categories: Vec<String>) -> Self;
    pub fn set_filter(&mut self, filter: String);      // resets scroll + cursor
    pub fn set_category(&mut self, cat: Option<String>);
    pub fn filter(&self) -> &str;
    pub fn filtered(&self) -> impl Iterator<Item = (usize, &PickerItem)>;
    /// Up/Down/Enter/Escape. Enter with no cursor picks filtered[0] when the
    /// filter is non-empty (type-and-enter, the fast path on stage).
    pub fn key_nav(&mut self, key: Key) -> PickerNav;
    pub fn cursor(&self) -> Option<usize>;
}
```

Filtering behavior is a verbatim move of `rebuild_filtered_list`
(`browser_popup.rs:287`): case-insensitive substring over `search_text.unwrap_or(label)`,
category pre-filter. `key_nav` is NEW behavior (the browser has no arrow-key nav
today — Escape only, `browser_popup.rs:759`): arrows move the cursor with wrap,
Enter picks. **What this means on stage: click Add, type three letters, Enter —
an effect lands without the mouse ever finding a grid cell.**

The browser's grid *rendering* consumes `filtered()` + `cursor()` (cursor = the
highlighted cell) and stays in `browser_popup.rs` untouched in structure.

## 5. Phasing

### P1 — Reproduce, then land the session contract + owned text sessions

- **Entry state:** clean clippy on the branch tip; `rg -n "session" crates/manifold-ui/src/panels/browser_popup.rs` shows no session struct yet; anchors in §1 re-verified (`browser_popup.rs` open/close line numbers may drift — re-run `rg -n "pub fn open|pub fn close" crates/manifold-ui/src/panels/browser_popup.rs`).
- **Read-back:** this doc §2–§3; `OVERLAY_SYSTEM_DESIGN.md` "Refinements" + "Status"; `text_input.rs` header comment. Restate: the layering rule (ui crate never sees TextInputState), the no-close-hook rule, and the three begin sites found by the entry check.
- **First step, before any fix:** reproduce Peter's symptom in the running app (open browser, search, pick / dismiss, observe the leftover text + the extra click). Write down the exact repro in the phase notes — the fix must be verified against the observed failure, not the inferred one. If the repro shows a *different* mechanism than the orphaned SearchFilter session, escalate with the observation before coding.
- **Deliverables:** `BrowserSession` split per §3 (all three modes); `TextSessionOwner` + `begin_owned` + `cancel_if_owned_by`; `take_closed_overlays()` on `UIRoot` + the app pump for both windows; the three SearchFilter sites converted.
- **Gate (positive):** manual repro from step 1 now clean — search text gone on close, no extra click needed, in BOTH windows (main-window effect browser, editor node picker); `cargo clippy --workspace -- -D warnings`; `cargo test -p manifold-ui --lib`.
- **Gate (negative):** `rg -n "\.clear\(\)" crates/manifold-ui/src/panels/browser_popup.rs` → 0 hits inside `close()` (close is one assignment); `rg -n "begin\(\s*crate::text_input::TextInputField::SearchFilter" crates/manifold-app/src` → 0 hits (all owned).
- **Forbidden moves:** keeping `close()`'s field clears "just in case" alongside the session drop (parallel old path) · giving the Overlay trait a close hook · clearing the text session from inside `manifold-ui` · widening into dropdown/ableton_picker (D4).
- **Test scope:** focused (`-p manifold-ui --lib`, `-p manifold-app` compile) + the manual repro. No parity, no workspace sweep — no render-path surface.

### P2 — Extract PickerCore; browser popup rides it; keyboard nav lands

- **Entry state:** P1 merged; `rg -n "struct BrowserSession" crates/manifold-ui/src/panels/browser_popup.rs` → 1 hit.
- **Read-back:** §4; `browser_popup.rs` `rebuild_filtered_list` + `handle_click`; the three open sites (`ui_root.rs:1420-1490`, `app_render.rs:1950-1998` — re-verify lines).
- **Deliverables:** `picker_core.rs` per §4 with unit tests (filter, category, nav wrap, type-and-enter); `BrowserSession.picker: PickerCore`; `BrowserPopupRequest` carries `Vec<PickerItem>` (D5) — the three open sites converted; arrow/Enter nav wired in both windows' key paths.
- **Gate (positive):** manual pass per mode (Effect / Generator / Node): open → type → chip → arrow-select → Enter places the right item; Escape dismisses; paste button unaffected. `cargo test -p manifold-ui --lib` (including the new picker_core tests); clippy.
- **Gate (negative):** `rg -n "to_lowercase" crates/manifold-ui/src/panels/browser_popup.rs` → 0 hits (filtering lives in picker_core only); `rg -n "item_names|item_type_ids|item_categories" crates/manifold-ui/src crates/manifold-app/src` → 0 hits (PickerItem replaced the parallel vecs).
- **Forbidden moves:** leaving the old filter code as a fallback · making PickerCore render anything · migrating dropdown/ableton_picker (D4) · changing grid visuals (this phase is behavior-neutral except the new keyboard nav).
- **Test scope:** focused ui lib + manual pass. Workspace sweep not triggered (UI-crate scope).

## 6. Decided — do not reopen

1. Session struct per overlay; close = drop. No reset lists.
2. Text sessions tagged with `TextSessionOwner`; app pump cancels on owner close.
3. PickerCore = model + interaction only; rendering stays per-surface.
4. Browser popup is the only migration in this design (dropdown/ableton deferred).
5. Items travel as `Vec<PickerItem>`, not parallel vectors.
6. No Overlay-trait close hook; no ui-crate access to TextInputState.

## 7. Deferred

- **Dropdown + Ableton picker onto the session contract / PickerCore.** Trigger:
  the next stale-state or focus bug in either, or the next feature that touches
  their internals.
- **TextInputState's own session collapse** (its five per-session `Option` context
  fields into one session enum/struct — same class, one level down; ~70 call
  sites). Trigger: the next stale-context bug in any text field, or any phase that
  already reworks `window_input.rs`'s key routing.
- **Settings popup / audio setup onto sessions.** Trigger: first bug or first
  feature touching their open-state.
- **Declarative overlay content DSL** — already deferred by OVERLAY_SYSTEM_DESIGN;
  unchanged.
