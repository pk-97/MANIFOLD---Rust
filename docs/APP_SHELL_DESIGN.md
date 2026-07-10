# App Shell — menus, settings, and the furniture around the instrument

**Status:** APPROVED design, not built · 2026-07-06 · Fable (with Peter in the room)
**Prerequisites:** none for P1–P3 (all phases run against shipped code: the muda menu, the overlay driver, the Chrome API). Future-wave slots (§8) bind their own waves, not this doc.
**Execution contract:** read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.

The governing insight: **MANIFOLD's shell is scattered, not missing.** A native menu bar, a settings popup, a config-modal substrate (the overlay driver), and a declarative panel API all ship today — but the menu stops at File, the settings popup holds four rows while a dozen real settings have no UI at all, `ProjectSettings` mixes four storage scopes in one serialized struct, and every keyboard shortcut past ⌘S is invisible. This doc commits the taxonomy — which surface owns which kind of control, which store owns which scope of setting, and how commands are named — so the existing pieces get completed rather than reinvented, and so the four future waves that each need a config surface (multi-display, projection mapping, LED, media backend) inherit a contract instead of inventing idioms.

Peter's directives, verbatim, where they decided something:

- Settings surfaces are **split** app/project (D2): Peter chose "Split: Preferences + Project Settings" over the recommended single scope-labeled window (2026-07-06). Both positions recorded at D2.
- Perform-mode entry stays menu-only: **"I think it's best we tuck the perform mode away in the menu so it can't accidentally be clicked and entered or exited."** (2026-07-06). This generalizes into taxonomy rule R5 (§7).

Companions: `CHROME_API_DESIGN.md` (the widget substrate every shell panel builds on) · `OVERLAY_SYSTEM_DESIGN.md` (the modal/modeless driver settings windows ride) · `UI_LAYOUT_DESIGN.md` + `UI_DESIGN_SYSTEM_AND_INSPECTOR_REDESIGN.md` (layout SSOT + tokens/kit; this doc adds no visual language) · `PERFORM_SURFACE_DESIGN.md` (perform modes constrain what furniture appears when; its §7.5 owns editor workspaces) · the future-wave docs reconciled in §8.

---

## 1. Audit — what exists (verified 2026-07-06)

Evidence: full reads of the files below plus headless renders (`cargo xtask ui-snap all` — timeline/inspector/states PNGs read this session). Extend, don't redesign.

| Piece | Where | State |
|---|---|---|
| Native menu bar (muda; macOS NSMenu) | `crates/manifold-app/src/menu.rs` | SHIPPED. MANIFOLD/File/Edit/View. One dispatch path: `MenuAction` → same `PanelAction` queue as chrome (`app_render.rs:915-919`). Dynamic Open Recent + Revert to Snapshot submenus. Edit = Undo/Redo only, **no accelerators** — menu.rs:11-17 names the constraint: macOS routes menu key-equivalents app-wide before winit, so context keys were deliberately left with winit "as a follow-up". |
| Settings popup (⌘,) | `crates/manifold-ui/src/panels/settings_popup.rs` | SHIPPED. One "RENDER" section, 340px modal: Resolution (opens picker), Render Scale 1×/75%/50%, Tonemap ×4, HDR Export. Chrome-API views + imperative rows. |
| Audio Setup modal (⌘⇧A) | `crates/manifold-ui/src/panels/audio_setup_panel.rs` (2090 lines) | SHIPPED. Device row, send rows (channel/gain/delete-with-confirm showing driven-param count), add-send, spectrogram scope with draggable band dividers, live-trigger band rows. Modal **by decision** (AUDIO_INFRASTRUCTURE §7: "stays modal … settings, configured deliberately"). Its UX roadmap is AUDIO_SENDS_UX_DESIGN — not this doc's to touch. **Superseded again (coherence audit F12, 2026-07-10): `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md` (approved 2026-07-09) amends this row's T2 classification to T1 — the panel becomes a `ScreenLayout` workspace column, not a modal overlay. This row and Decided-#9 below are stale until that design's P1 lands.** |
| Overlay driver | `crates/manifold-app/src/ui_root.rs` (`OverlayId`: PerfHud/Dropdown/AudioSetup/Settings/BrowserPopup/AbletonPicker/Toast) | SHIPPED (OVERLAY_SYSTEM_DESIGN). Build/draw/input from one enum; adding an overlay = a variant. The substrate for both settings windows. |
| Perf HUD (backtick) | `crates/manifold-ui/src/panels/perf_hud.rs` | SHIPPED. FPS/frame-graph/sync/MIDI/clip metrics, modeless, never consumes input. |
| Transport bar | `crates/manifold-ui/src/panels/transport.rs` | SHIPPED on Chrome API. Left: clock authority, Link, MIDI clock (CLK + device), SYNC out. Center: PLAY/STOP/REC, BPM field + R + CLR. Right: automation LANES/BACK/ARM. |
| Header bar | `crates/manifold-ui/src/panels/header.rs` | SHIPPED on Chrome API. Project name + import status (left), time display (center), zoom −/label/+ (right). No mode buttons — Perform/Monitor/Audio are menu-only (see D4). |
| Footer bar | `crates/manifold-ui/src/panels/footer.rs` | SHIPPED on Chrome API. Quantize cycle, selection info, FPS field, layer/clip counts. |
| Project-scope settings | `crates/manifold-core/src/settings.rs` (`ProjectSettings`) | SHIPPED, **a four-scope grab-bag**: project proper (resolution/fps/bpm/time-sig/render-scale/tonemap/vsync/export_hdr), workspace state (inspector_width, viewport scroll/zoom, collapse flags), venue-class rig config (led_* fields, `midi_clock_source_name`, `osc_send_port`/`osc_sync_mode`), and 17 `legacy_*` Unity fields. |
| App-scope prefs | `crates/manifold-app/src/user_prefs.rs` (`UserPrefs`) | SHIPPED, Unity-port string KV (`prefs.json`). Keys in use (rg, 2026-07-06): `MANIFOLD_RecentProjects`, `MANIFOLD_LastOpenedProjectPath`, `MANIFOLD_LastExportFileName`, `MANIFOLD_Export`, `MANIFOLD_Frame`, `MANIFOLD_DialogPath_*`. No typed app-settings struct exists. |
| Settings with **no UI at all** | `settings.rs` | `vsync_enabled`, `osc_send_port`, `osc_sync_mode`, `video_player_pool_size`, `max_layers`, `default_recording_layer` — JSON-only today. |
| Autosave | `crates/manifold-app/src/autosave.rs` (60s debounce const) + `manifold-io/src/archive.rs` (`DEFAULT_MAX_AUTO_SAVES` 50) | SHIPPED (GIG_RESILIENCE P1); cadence and cap are hard-coded constants, no UI. |
| Context keyboard shortcuts | `crates/manifold-app/src/input_handler.rs` | SHIPPED, winit-only, invisible: ⌘Z/⇧⌘Z/⌘Y, ⌘S/O/N, ⌘A/C/X/V (context-sensitive clips-vs-effects, Finder-paste arbitration), ⌘D, ⌘G/⇧⌘G, ⌘E split-at-playhead, arrow nudges. No shortcut reference surface anywhere. |
| Windows | `crates/manifold-app/src/window_registry.rs` (`WindowRole`) | SHIPPED: exactly `Workspace` + `Output { presentation }`. Graph editor and perform mode are in-window surfaces of Workspace, not OS windows. "Monitor" (View menu) = toggle the output window (`pending_toggle_output`, `app_render.rs:1277`). |
| Command surface for agents | `docs/MCP_INTERFACE_DESIGN.md` | APPROVED not built. Its rule "catalog = the live registry, never a separately maintained doc" is the precedent D6 applies to commands. |

Negative claims verified by search (2026-07-06): no MIDI-mapping settings UI and no OSC settings UI exist in `manifold-ui` (`rg osc_send_port|MidiMappingConfig crates/manifold-ui/` — zero hits); no `NSMenu` use outside `menu.rs`; no third window role.

**What the audit says about scope:** almost everything is *one wire away from existing*. The genuinely new pieces are: the command table (D6), the two settings windows' sidebar shell (one new T2 component), and the typed `AppPrefs` store (D3). Everything else is completion and re-homing.

## 2. Decisions

**D1 — The shell completes what ships; it does not restructure it.** The muda menu, the overlay driver, the Chrome API, and the chrome bars are the architecture. No new window roles, no new input system, no docking framework (PERFORM_SURFACE §7.5 owns workspaces, explicitly deferred there). Rejected: a dock/panel-management framework "while we're at it" — that is the cascade `feedback_dont_cascade_redesign` forbids, and R1 (steal-pass) already rejected DIY UI toolkits.

**D2 — Two settings windows, split by scope (Peter's call, 2026-07-06).** **Settings…** (⌘,, MANIFOLD menu) holds app-scope configuration; **Project Settings…** (File menu) holds project-scope configuration. Venue-scope config never gets a third window — it lives on venue surfaces owned by their waves (§8). Rejected: one window with scope-labeled pages (the session's recommendation — users think in domains, one home for "where do I set X"; recorded here because a future session will reinvent it). Peter chose the Resolve/Resolume convention; the mitigation for the two-homes cost is R6 (§7): every settings row states its scope in its page header, and §6 fixes each row's home so no future wave re-litigates per setting.

**D3 — App scope gets a typed store: `AppPrefs`.** A serde struct in `manifold-app` persisted at the platform config dir (same directory as today's `prefs.json`), replacing the Unity-port string KV. The six existing `MANIFOLD_*` keys load-migrate into typed fields; `UserPrefs` is deleted at the end of P3 (negative gate). Rejected: keeping `UserPrefs` and layering typed accessors over string keys — that keeps the stringly-typed bug class alive (`feedback_eliminate_bug_class_at_storage_layer`) for a one-session saving.

**D4 — Perform-mode entry is menu-only, deliberately (Peter, verbatim above).** No header/transport PERFORM button, no accelerator. An accidental mode switch mid-set is a show-killer; entry must be a deliberate act. The triple-redundant *exit* ladder (perform_mode safety rules) is untouched. Monitor and Audio Setup keep their menu entries (Audio Setup also keeps ⌘⇧A — opening a config modal is recoverable in a way a mode switch is not).

**D5 — The menu owns its accelerators, context-routed.** The Edit menu is completed (Cut/Copy/Paste/Duplicate/Delete/Select All/Group/Ungroup/Split at Playhead) and each item carries its real key equivalent. A claimed key's winit branch in `input_handler.rs` is **deleted in the same phase** — the menu fires the `MenuAction`, which routes through the *same context-sensitive dispatch* the key branch ran (clips vs effects vs active text-input session). This is the root fix for the dual-key-path split menu.rs itself flagged as a follow-up. Rejected: display-only shortcuts (macOS key equivalents always claim; there is no display-only mode) and the status quo (Edit stays a stub; shortcuts stay undiscoverable). Named hazard: **double-fire** — a key alive in both paths fires twice; the deletion is part of the definition of claiming a key, not cleanup.

**D6 — Commands are data: one static table.** `commands.rs` in `manifold-app`: id, menu placement, display title, accelerator, lowering. The menu is **built from the table**; the Help → Keyboard Shortcuts overlay renders the table; the future MCP command surface reads the table. Dispatch is unchanged — lowerings produce the existing `MenuAction`/`PanelAction`/context routing; no enablement predicates, no command-palette architecture. This is MCP_INTERFACE's "catalog = live registry" rule applied to commands, scoped by inventory: the disease observed is *naming/shortcut truth duplicated across menu.rs, input_handler.rs, and any future Help page*; dispatch is already unified through the one PanelAction queue, so a Blender-style registry with enablement/context would be cascade, not root fix. Rejected: full command registry with enablement (no observed disease it cures); hand-extending menu.rs (the duplication grows with every item).

**D7 — Settings apply live; nothing requires a restart.** Every row in both windows dispatches through the existing action paths (`PanelAction` → `ui_bridge` → `EditingService`/direct app state) the moment it changes. No OK/Apply/Cancel. A future setting that genuinely cannot apply live escalates to Peter before shipping — "restart required" is not a pattern this app acquires. (Consequences, stated honestly: live-apply on things like player pool size means the change may take effect progressively (pool grows/shrinks on next acquire); rows whose effect is deferred must say so in their sublabel, not pretend immediacy.)

**D8 — Workspace state stays in the project file.** `inspector_width`, viewport scroll/zoom, collapse flags remain in `ProjectSettings` — a project reopening the way you left it is DAW behavior (Ableton does exactly this), not scope pollution. The venue-class fields (`led_*`, `osc_*`, `midi_clock_source_name`) also **stay put for now**: MULTI_DISPLAY #13 owns the venue-profile store (display-identity-keyed, exportable venue file) and LED_STRIPS D5 already commits the LED migration into it. This doc classifies (§6.3) and does not migrate — a storage move here would collide with that owned work. The 17 `legacy_*` fields are load-migration surface, owned by manifold-io, untouched.

**D9 — The two settings windows are one shell component.** A T2 modal overlay (popup_shell + Chrome API, sized like Audio Setup's viewport-fraction mode) with a left sidebar of pages and a page body; instantiated twice with different page sets. Pages are `View`-tree functions on panel state — the same pattern `settings_popup.rs` already uses, generalized. Each page header states its scope in one quiet line ("Saved with this project" / "This computer"). Rejected: a separate OS window (no third `WindowRole` exists; the one-UI-tree-per-workspace model makes an in-app modal the house pattern and the overlay driver makes it a registration).

## 3. The scope model — who stores what

Four scopes, three stores, no exceptions:

| Scope | Store | Examples | UI home |
|---|---|---|---|
| **App** (this computer) | `AppPrefs` (new, D3) | recent projects, last dialog paths, autosave cadence/cap, license (future) | Settings… ⌘, |
| **Project** (travels with the show) | `ProjectSettings` (exists) | resolution, fps, render scale, tonemap, vsync, HDR export, bpm/time-sig, OSC/sync config, pool sizes | Project Settings… + on-surface quick controls |
| **Workspace** (project file, D8) | `ProjectSettings` | inspector width, viewport scroll/zoom, collapse states | no settings UI — the surfaces themselves |
| **Venue** (the copper and glass) | venue profile (MULTI_DISPLAY #13, future) | stage layout, projector warps, LED patch | venue surfaces (§8), never a settings window |

### Committed shape — `AppPrefs`

```rust
// crates/manifold-app/src/app_prefs.rs — replaces user_prefs.rs (P3)
#[derive(Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct AppPrefs {
    pub recent_projects: Vec<PathBuf>,          // ← MANIFOLD_RecentProjects
    pub last_opened_project: Option<PathBuf>,   // ← MANIFOLD_LastOpenedProjectPath
    pub last_export_file_name: Option<String>,  // ← MANIFOLD_LastExportFileName
    pub dialog_paths: HashMap<String, PathBuf>, // ← MANIFOLD_DialogPath_*
    pub autosave_debounce_secs: u32,            // ← autosave.rs const (default 60)
    pub autosave_history_cap: u32,              // ← archive.rs const (default 50)
}
```

Persisted as `settings.json` beside the legacy `prefs.json`; on first load, if `settings.json` is absent and `prefs.json` exists, migrate the keys and leave `prefs.json` in place (a downgrade still works); `UserPrefs` the *type* is deleted. `⚠ VERIFY-AT-IMPL:` the exact key set — re-run `rg -o '"MANIFOLD_[A-Za-z_]*"' crates/manifold-app/src/` before writing the migration; the table in §1 is the 2026-07-06 snapshot. (`MANIFOLD_Export`/`MANIFOLD_Frame` at `app_lifecycle.rs:127/215` — read those call sites to type them correctly.)

## 4. The command table

```rust
// crates/manifold-app/src/commands.rs — the single source of command truth (P1)
pub struct CommandDef {
    pub id: &'static str,             // "edit.copy" — stable, dot-namespaced
    pub title: &'static str,          // "Copy" — menu text, product copy
    pub menu: Option<MenuPlacement>,  // menu + position; None = shortcut-only (e.g. arrow nudges)
    pub accel: Option<&'static str>,  // muda accelerator string; None = no key
    pub lower: Lowering,              // how it dispatches
}
pub enum Lowering {
    Menu(MenuAction),        // existing file/view ops — unchanged path
    Panel(PanelAction),      // direct single action
    Context(EditIntent),     // context-routed: the dispatcher input_handler already is
}
pub enum EditIntent { Cut, Copy, Paste, Duplicate, Delete, SelectAll, Group, Ungroup, SplitAtPlayhead, Undo, Redo }
```

`menu.rs::build()` walks the table instead of hand-appending items (dynamic submenus — Open Recent, Revert to Snapshot — stay bespoke; they are data-driven already). `input_handler.rs`'s claimed-key branches become one `EditIntent` router the menu drain also calls; the router's **first check is the active text-input session** (a ⌘C while typing in the BPM field must hit the text field, exactly as the key branch orders it today). Keys the menu does not claim (arrows, space, backtick, Escape) stay winit and may still appear in the table (`menu: None`) so the shortcuts overlay lists them.

## 5. The menu bar — committed structure

Naming rules (product copy, binding): Title Case per macOS HIG; verb-first; `…` exactly when a dialog or window follows; no branding in item names; an item that toggles states its noun ("Performance HUD"), not "Toggle X". The table below is the contract — an executor adds nothing to it and omits nothing from it without escalation.

```
MANIFOLD   About MANIFOLD · ─ · Settings… ⌘, · ─ · Services · Hide/Hide Others/Show All · ─ · Quit
           [slot: Check for Updates… — COMMERCIALIZATION updater, directly under About]
File       New ⌘N · Open… ⌘O · Open Recent ▸ · ─ · Save ⌘S · Save As… ⇧⌘S · Revert to Snapshot ▸ · ─ ·
           Project Settings… · ─ · Import Video… ⌘I · Export Video… · Export Frame…
           [slots: Import Scene… (IMPORT wave) · Import Ableton Set… (ABLETON_SHOW_SYNC) — join a File ▸ Import
            submenu only when a third import kind exists; two items stay flat]
Edit       Undo ⌘Z · Redo ⇧⌘Z · ─ · Cut ⌘X · Copy ⌘C · Paste ⌘V · Duplicate ⌘D · Delete ⌫ · Select All ⌘A · ─ ·
           Group ⌘G · Ungroup ⇧⌘G · ─ · Split at Playhead ⌘E
View       Perform Mode  (no accelerator — D4) · ─ · Audio Setup ⇧⌘A · Performance HUD (`) · ─ ·
           Zoom In · Zoom Out
           [slots: Session View (SESSION_MODE) · Stage View (MULTI_DISPLAY) · Mapping (PROJECTION_MAPPING)]
Window     Minimize ⌘M · Zoom · ─ · Monitor
           [slot: per-output window list (MULTI_DISPLAY P3 multi-output present)]
Help       Keyboard Shortcuts… · [slot: MANIFOLD Manual — when public docs exist (BUSINESS_PLAN §7)]
```

Decisions folded in: **Monitor moves View → Window** (it is a window, not a view state; macOS convention). Zoom In/Out get menu presence (they exist as header buttons; the menu item makes them discoverable — accelerators `⌘+`/`⌘-` are claimed by the menu per D5, deleting any winit equivalents). Redo standardizes on ⇧⌘Z in the menu; the ⌘Y winit alias survives as a table entry with `menu: None`. Export Frame/Export Video move under File exactly as they are (already `MenuAction`s). `PanelAction::ExportXml` exists in the enum — `⚠ VERIFY-AT-IMPL:` `rg 'ExportXml' crates/manifold-app/src/` — if it has no live emitter, it does not get a menu item (dead actions don't get furniture).

**Keyboard Shortcuts… (Help)** is a T3-adjacent modal overlay (popup_shell, scrollable, Esc closes) rendering the command table grouped by menu — plus the winit-only entries (arrows, space, Escape chain) from their `menu: None` rows. Zero hand-maintained content: it *is* the table, which is what keeps it true.

## 6. The two settings windows

Both are instances of the D9 sidebar shell. Every row dispatches live (D7) through existing action paths; new rows for currently-UI-less settings get `PanelAction`s routed like their siblings (`SetRenderScale` et al. are the worked example — `ui_bridge` → `MutateProject`).

### 6.1 Settings… (⌘,) — app scope

| Page | Rows (v1) | Notes |
|---|---|---|
| **General** | Autosave: debounce interval (30s/60s/2min/5min segmented) · history cap (numeric, default 50) · "Clear Recent Projects" button | The two autosave constants become `AppPrefs` fields (§3); the archive keeps enforcing the cap. |
| *slot:* **Audio** | default input device for new projects | Lands with AUDIO_INFRASTRUCTURE's device directory (its Phase 2+), not before — no page ships empty. |
| *slot:* **License & Updates** | registration, update channel | COMMERCIALIZATION_DESIGN's surface; it lands the page with its wave. |

v1 Settings is one page. That is honest — MANIFOLD has few app-scope settings today, and a page that exists to look complete is forbidden (standard §7). The window's value is the idiom the slots inherit.

### 6.2 Project Settings… (File) — project scope

| Page | Rows (v1) | Source |
|---|---|---|
| **Video** | Resolution (picker) · Frame Rate (numeric — same field the footer edits) · Render Scale 1×/75%/50% · Tonemap ×4 · VSync toggle · HDR Export toggle | First four re-homed from `settings_popup.rs` (then the popup is **deleted** — no parallel path); VSync + HDR wired to existing fields (`ToggleHdr` exists; VSync needs a new action). |
| **Playback** | Video player pool size · Max layers · Default recording layer | The JSON-only trio gets its first UI. Sublabels state deferred effect where true (D7). |
| **Sync** | OSC send port · OSC sync mode | Cold config only. Clock authority, Link, MIDI-clock device, SYNC out **stay in the transport bar** — they are live show state (R2, §7). Quantize stays in the footer for the same reason. |
| *slots:* **Displays & Stage** (MULTI_DISPLAY: canvas/stage summary + venue file import/export entry) · **LED** (LED_STRIPS: patch summary + link to patch surface) · **Media** (MEDIA_BACKEND: active decode backends, diagnostics) | Each lands with its wave; the sidebar order is fixed now: Video · Playback · Sync · Displays & Stage · LED · Media. |

### 6.3 Classification of today's misfiled fields (no storage migration — D8)

`led_exit_index/brightness/gain/enabled`, `midi_clock_source_name`: venue-class, stay in `ProjectSettings` until MULTI_DISPLAY #13's venue profile exists; their UI stays where it is (Master chrome / transport). `osc_send_port`/`osc_sync_mode`: project-scope (a show's OSC contract travels with it) — Sync page, above. This paragraph is the record future waves consult so the classification isn't re-derived.

## 7. Panel taxonomy — the contract

Five tiers plus windows. Every existing surface classified; every future surface must name its tier in its own design doc.

| Tier | What | Members today | Input/host |
|---|---|---|---|
| **T0 — chrome bars** | fixed furniture, always visible in edit mode | transport, header, footer | Chrome API panels, `ScreenLayout` SSOT |
| **T1 — workspace surfaces** | the working canvases; dock per `ScreenLayout` | preview, inspector, timeline, graph editor · future: session grid, stage view, mapping panel | Panel trait; View menu entries |
| **T2 — modal config overlays** | deliberate configuration; scrim; Esc closes | Settings, Project Settings, Audio Setup | overlay driver, `Modality::Modal` |
| **T3 — modeless utility overlays** | glanceable, never capture input | perf HUD, toasts, Keyboard Shortcuts | overlay driver, `Modality::Modeless` |
| **T4 — transients** | opened-from-something, dismiss-on-outside-click | dropdowns, pickers, browser popup | overlay driver / popup_shell |
| **Windows** | OS windows | Workspace + Output(s) — exactly two roles | `WindowRegistry` |

Audio Setup's T2 row is superseded going forward: `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
(approved 2026-07-09) reclassifies it T1 (a `ScreenLayout` workspace column) per this
doc's own R3 — coherence audit F12, 2026-07-10.

Placement rules (R-numbered; these are what future waves inherit):

- **R1 — Live controls live on the performance surface.** Anything a performer touches mid-set (BPM, quantize, sync arming, LED master, sends' gains) is on a T0 bar, the inspector, or a perform widget — never inside a T2 window. `feedback_param_values_is_performance_surface` and `feedback_audio_stays_on_perform_surface` are the governing memories.
- **R2 — A control may have two homes, one truth.** A quick surface (footer FPS) and a settings row (Video page) may both edit one field; they must dispatch through the same action. Two homes, two actions, is the forbidden version.
- **R3 — Tune-while-watching config is a T1 surface, not a modal.** Projection warp, stage arrangement, LED test patterns are used while looking at the physical stage with content running — a scrim would blind exactly the thing being tuned. (PROJECTION_MAPPING §6 already commits its "Mapping dock panel … usable while content runs"; this rule generalizes it.)
- **R4 — Set-and-forget config is a T2 page** in the window its scope dictates (D2).
- **R5 — Show-disruptive mode switches get low-affordance entries** (menu item, no accelerator, no chrome button). Peter's perform-mode directive, generalized. Exits stay redundant and easy; entries stay deliberate.
- **R6 — Every settings page header states its scope** ("Saved with this project" / "This computer" / venue name) — the mitigation for the split-window cost recorded at D2.
- **R7 — New config surfaces are registrations, not inventions.** A wave adds: a sidebar page (T2), a workspace surface + View menu item (T1), or a command-table row — never a new modal type, a third settings window, or a bespoke input path. A wave that thinks it needs one escalates.

## 8. Future-wave slot contracts (reconciled 2026-07-06 against each doc)

| Wave | What it adds to the shell | Tier / home | Reconciliation |
|---|---|---|---|
| MULTI_DISPLAY | Stage View (arrangement, test patterns, Identify, per-output advanced flap) · venue file import/export · output windows | T1 surface + View menu · Project Settings ▸ Displays & Stage summary page · Window menu list | **AMENDED 2026-07-06 (its §5a):** Stage View is the app's ONE physical-stage surface — hosts projector and LED-fixture objects too; deep tools open as focused modes with breadcrumb. |
| PROJECTION_MAPPING | Mapping surface (Warp/Masks/Blend) | Focused mode inside Stage View (no own panel) | **AMENDED 2026-07-06 (its §6 home amendment):** entered by selecting a projector on the Stage; tools/math verbatim. |
| LED_STRIPS | Fixture patch (venue profile), protocol outputs | Fixture objects on Stage View; patch detail = focused mode (its §5a) + Project Settings ▸ LED summary/link | **AMENDED 2026-07-06:** no separate patch panel. D5 venue-profile storage untouched; §6.3 records the interim field locations. |
| MEDIA_BACKEND | none committed in its doc | Project Settings ▸ Media diagnostics slot reserved (backends active, codec info) | Reserve only — no UI is invented for it here. |
| AUDIO_INFRASTRUCTURE / AUDIO_SENDS_UX | Audio Setup evolution (rename/color sends, meters, grouped channels) · default-device pref | Audio Setup stays T2 modal (its §7 decision, quoted in §1) · Settings ▸ Audio slot | This doc adds nothing to Audio Setup; the sends UX doc owns it. |
| SESSION_MODE / PERFORM_SURFACE | Session View toggle · perform surfaces · editor workspaces | View menu slot · perform is its own chrome-hosted mode (D2 there) · workspaces = PERFORM_SURFACE §7.5's own future design | Perform entry affordance decided here (D4); everything else is theirs. |
| COMMERCIALIZATION | About/registration, Check for Updates…, license page | MANIFOLD menu slots + Settings ▸ License & Updates page | Its doc names "about/registration panel"; the slots above are the landing sites. |
| GIG_RESILIENCE | (already landed its shell pieces) | File ▸ Revert to Snapshot — shipped | Nothing owed. |
| MCP_INTERFACE | command surface | reads the D6 command table | Its "catalog = live registry" rule is D6's precedent; when it lands a `commands` MCP tool, the table is the source. |

## 9. Phasing

Entry state, every phase: re-verify the §1 anchors the phase touches (each brief lists its own). Test scope per phase, verified once at phase end. All three phases are `manifold-app`/`manifold-ui` only — no content-thread work, no GPU-path work (no gpu-proofs runs; no render-trace gates — nothing here adds content-thread load).

### P1 — Command table + the full menu (one session)

- **Entry state:** `menu.rs` builds by hand (`rg 'fn build' crates/manifold-app/src/menu.rs`); Edit menu has 2 items; `input_handler.rs` owns ⌘X/C/V/D/A/G/⇧⌘G/⌘E (`rg 'Cmd\+' crates/manifold-app/src/input_handler.rs` matches §1's list).
- **Read-back:** this doc §2 D5/D6, §4, §5; `menu.rs` whole; `input_handler.rs:60-230`; restate the double-fire hazard and the text-input-first routing rule before writing code.
- **Deliverables:** `commands.rs` (table + `EditIntent` + router); `menu.rs` building from the table; Edit/View/Window/Help completed per §5 (including Monitor → Window, Zoom items); winit branches for claimed keys **deleted**; Keyboard Shortcuts overlay (new `OverlayId` variant, popup_shell, renders the table).
- **Gate (positive):** unit test: every `CommandDef` with `menu: Some` appears exactly once in the built menu with its title and accelerator; every `EditIntent` has exactly one table row with an accelerator. Focused `cargo test -p manifold-app --lib` + `-p manifold-ui --lib`; clippy workspace.
- **Gate (negative):** `rg 'is_cmd_x|cmd.*Key::KeyC|Key::KeyV' crates/manifold-app/src/input_handler.rs` (exact patterns re-derived from the deleted branches) = 0 for every claimed key — the old path is gone, not paralleled. `rg 'Submenu::new\("Edit"' crates/manifold-app/src/menu.rs` = 0 (menu built from table, not hand-appended).
- **Acceptance demo (L2 + Peter click-script):** the native menu cannot be driven headlessly — stated, not hidden. Artifact: PNG of the Keyboard Shortcuts overlay via a new `ui-snap` scene (`shortcuts`). Click-script for Peter (≤2 min): (1) select a clip, ⌘C ⌘V — pastes once, not twice; (2) click into the BPM field, type, ⌘C ⌘V — text copies/pastes, no clip paste; (3) Edit menu shows the full item set with keys; (4) Help ▸ Keyboard Shortcuts… lists them; (5) View ▸ Perform Mode still enters perform (menu path intact).
- **Forbidden moves:** keeping a winit branch "as fallback" for a claimed key (double-fire) · a second hand-maintained shortcut list anywhere (the overlay renders the table or fails) · adding enablement/context fields to `CommandDef` (D6 scope) · touching perform-mode entry affordances (D4).
- **Test scope:** focused libs + clippy; this is UI-infrastructure → full workspace sweep before merge.

### P2 — Project Settings window (one session)

- **Entry state:** P1 merged (File menu builds from table). `settings_popup.rs` exists with its 4 rows; `OverlayId::Settings` routes ⌘,; `rg 'vsync_enabled|video_player_pool_size|osc_send_port' crates/manifold-ui/` = 0 (still no UI).
- **Read-back:** §2 D7/D9, §6.2, §7 R1/R2; `settings_popup.rs` whole; `audio_setup_panel.rs` sizing/`resize_to_viewport`; the `SetRenderScale` dispatch chain end-to-end (`ui_bridge`) as the worked example for new rows.
- **Deliverables:** sidebar-shell component (`manifold-ui`, Chrome API views: sidebar + scope-header + page body); Project Settings window on it with Video/Playback/Sync pages per §6.2; new `PanelAction`s (`SetVsync`, `SetPlayerPoolSize`, `SetMaxLayers`, `SetDefaultRecordingLayer`, `SetOscSendPort`, `SetOscSyncMode`) routed like `SetRenderScale`; File ▸ Project Settings… command-table row; **`settings_popup.rs` deleted**, `OverlayId::Settings` re-pointed to the new window (⌘, temporarily opens Project Settings until P3 lands Settings — say so in the phase report, it's a one-phase interim the next phase closes, not a silent fallback).
- **Gate (positive):** new `ui-snap` scene `project-settings` renders the window (Video page active) — PNG read by the orchestrating session; round-trip gate (standard §5): set VSync off + pool size 6 → save → reload → values persist AND the rows show them (this is serialized state; create-path green is half a gate). Focused libs + clippy.
- **Gate (negative):** `rg 'settings_popup' crates/` = 0 · `rg 'SECTION_H|"RENDER"' crates/manifold-ui/src/panels/` finds no orphaned popup remnants · no new `Arc<Mutex` (`rg 'Arc<Mutex' crates/manifold-ui/ crates/manifold-app/` count unchanged).
- **Acceptance demo (L2):** the `project-settings` PNG per page (Video/Playback/Sync). Click-script: (1) File ▸ Project Settings… opens; (2) flip VSync — playback pacing visibly changes mode (perf HUD frame graph); (3) Esc closes; (4) reload project — values held.
- **Forbidden moves:** keeping the old popup alive behind the old ⌘, path (parallel old path) · OK/Apply buttons (D7) · moving BPM/quantize/clock controls off the performance surface into the window (R1) · inventing settings not in §6.2's table · storage migration of venue-class fields (D8).
- **Test scope:** focused `-p manifold-ui --lib` + `-p manifold-app --lib`; clippy workspace; workspace sweep before merge (serialization-adjacent).

### P3 — Settings window + `AppPrefs` (one session)

- **Entry state:** P2 merged. `user_prefs.rs` exists; re-run the §3 key sweep (`rg -o '"MANIFOLD_[A-Za-z_]*"' crates/manifold-app/src/`) — if the key set differs from §1's snapshot, list the delta before coding.
- **Read-back:** §2 D2/D3/D7, §3, §6.1; `user_prefs.rs` whole; every `get_string`/`set_string` call site; `autosave.rs` + `archive.rs` constants.
- **Deliverables:** `app_prefs.rs` per §3 (typed struct, `settings.json`, one-time migration from `prefs.json`); every `UserPrefs` call site moved to typed fields; **`user_prefs.rs` deleted**; autosave debounce + history cap read from `AppPrefs`; Settings window (same shell, General page per §6.1); ⌘, re-pointed to Settings; MANIFOLD ▸ Settings… table row confirmed.
- **Gate (positive):** migration test: a fixture `prefs.json` with all six key kinds loads into the right typed fields; round-trip gate: change autosave interval → restart-load path (`AppPrefs::load`) returns it; autosave honors the new debounce (unit-test the debounce source, not wall-clock). Focused libs + clippy.
- **Gate (negative):** `rg 'UserPrefs|MANIFOLD_RecentProjects|get_string' crates/manifold-app/src/` = 0 (the string-KV path is gone, not wrapped).
- **Acceptance demo (L2):** `ui-snap` scene `settings` PNG (General page). Click-script: (1) ⌘, opens Settings, General shows autosave rows with "This computer" scope line; (2) File ▸ Project Settings… still opens the project window; (3) set autosave to 2min, quit, relaunch, reopen Settings — value held.
- **Forbidden moves:** keeping `UserPrefs` as a compatibility layer (D3 rejection) · seeding pages from §6.1's *slot* rows (no empty License/Audio pages) · writing `settings.json` schema without `#[serde(default)]` on every field (old files must load forever).
- **Test scope:** focused libs + clippy; workspace sweep before merge (persistence).

Future-wave slots (§8) are not phases of this doc — each lands inside its own wave, bound by R7.

## 10. Decided — do not reopen

1. Settings split app/project into two windows: Settings… ⌘, + Project Settings… (Peter, 2026-07-06). One-window-with-scope-labels was considered and rejected.
2. Perform-mode entry is menu-only, no accelerator, no chrome button (Peter, verbatim in header). Exit redundancy untouched.
3. Venue config never gets a settings window — T1 surfaces + venue profile (owned by MULTI_DISPLAY/LED waves).
4. The menu owns its accelerators; claimed keys' winit branches are deleted same-phase; context routing goes text-input-first.
5. Commands are a static data table (id/title/menu/accel/lowering) — no enablement predicates, no palette architecture, dispatch unchanged.
6. All settings apply live; no restart-required pattern; no OK/Apply.
7. Workspace state (widths, scroll, collapse) stays in the project file.
8. `AppPrefs` replaces `UserPrefs`; the string KV is deleted, not wrapped.
9. Audio Setup stays a modal (AUDIO_INFRASTRUCTURE §7's decision, honored here).
   **Superseded 2026-07-09 by `AUDIO_SETUP_DOCK_AND_TRIGGER_UNIFICATION_DESIGN.md`
   (T1 workspace column, not T2 modal) — audit F12, 2026-07-10.**
10. Monitor lives in the Window menu; exactly two `WindowRole`s remain.
11. Settings windows are in-app T2 overlays on the overlay driver, not OS windows.
12. Sidebar page order (Project Settings): Video · Playback · Sync · Displays & Stage · LED · Media.

## 11. Deferred (with revival triggers)

- **Settings search** — revive when either window exceeds ~6 live pages. The page-as-`View`-function pattern keeps rows enumerable; search is an added filter, not a rework.
- **Editor workspaces / docking** — PERFORM_SURFACE §7.5 owns it; gets its own design when scheduled.
- **Command palette (⌘K)** — revive if/when MCP's command surface proves the table's coverage; it would read the same table.
- **Menu-bar presence for session/stage/mapping views** — slots reserved (§5); land with their waves.
- **Window-title dirty indicator + represented file (macOS proxy icon)** — nice citizenship, zero urgency; bundle into whichever shell phase next touches `app_lifecycle.rs`, as a noted extra, not scope growth.
- **Venue-field storage migration out of `ProjectSettings`** — owned by MULTI_DISPLAY #13; §6.3 is the classification it consumes.
