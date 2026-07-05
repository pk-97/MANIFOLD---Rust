<!-- index: Getting files onto the timeline stops being a dice roll. Four fixes, one wave: (1) file-drop targeting reads the REAL pointer position during a Finder drag (winit never updates the cursor mid-drag, so today's join-onto-lane logic aims at a stale position and almost always spawns a new layer instead) plus a live highlight of the lane the file will land on; (2) Cmd+V pastes a file copied in Finder, arbitrated against the internal clip clipboard by NSPasteboard changeCount; (3) an audio clip's source file can be replaced in place (command + inspector gesture), keeping the clip, its lane, its detection config and routing, so re-detect reuses the whole set; (4) stem lanes are keyed by role (drums/bass/other/vocals) instead of by name, so replacing the song on a lane reuses the existing stem lanes and sends instead of spawning four more. -->

# Timeline Ingest — drop, paste, replace

**Status:** APPROVED design, not built · 2026-07-04 · Fable · **baseline-reviewed 2026-07-05, cleared** (zero unlabeled forks; anchors spot-reverified — symbols all hold, line drift only, e.g. drop arms app.rs:2388→~2447, SwapVideoCommand :338→:368; trust each phase's entry-state re-derivation. §10 levels: P1/P2 gates are L4 by nature — neither headless tests nor the UI-automation layer can synthesize an OS drag session; P3–P5 gate L1 with manual L4 extras.)
**Prerequisites:** none (extends shipped audio-clip-detection + drop paths)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

Peter, 2026-07-04: *"Dragging and dropping an audio file seems to create a new layer
instead of dropping the audio file onto the layer. Would be nice to be able to copy and
paste an audio file in like you can with Ableton too. Replacing the audio file and
running the analysis on that new clip reusing the same layers etc."*

The governing insight: **every one of these is a targeting/identity problem, not a
detection problem.** The join-onto-lane logic already exists and is correct; it aims at
a stale cursor. The lane-reuse machinery already exists and is correct; it looks lanes
up by a name that changes when the song changes. The fixes are small and root-cause:
give the drop path the real pointer, give the paste path the real pasteboard, give the
clip a real replace operation, give stem lanes a real identity.

Companion docs: `AUDIO_CLIP_DETECTION_DESIGN.md` (the detect/group machinery this
extends — its §8.3 lane-keyed reuse is the contract P5 here makes hold),
`AUDIO_LAYER_DESIGN.md` §6 (the original drop affordance).

## 1. Audit — what exists (verified 2026-07-04)

Extend, don't redesign. Every piece below is live on `main`.

| Piece | Where | State |
|---|---|---|
| Join-onto-lane drop logic | [project_io.rs:605-694](../crates/manifold-app/src/project_io.rs#L605) | Correct. Joins `join_audio_layer` if given, else appends a new audio lane. One undo step per file. |
| Drop-target resolution | [app.rs:2388-2412](../crates/manifold-app/src/app.rs#L2388) | **The bug.** Resolves lane + beat from `self.cursor_pos`, which winit last updated *before* the Finder drag began. |
| winit macOS drag events | winit-0.30.13 `platform_impl/macos/window_delegate.rs:369-429` | Implements only `draggingEntered` (→ `HoveredFile`, fires once) and the final drop. **No `draggingUpdated`, no position, no `CursorMoved` during a drag.** Verified in the vendored source. |
| Y→lane mapping | [coordinate.rs:102-116](../crates/manifold-ui/src/panels/viewport/coordinate.rs#L102) (`layer_at_y`) | Correct; delegates to the single-Y-authority mapper, rejects dead space below the last track. |
| Internal clip copy/paste | [service.rs:315-471](../crates/manifold-editing/src/service.rs#L315) (`copy_clips`/`paste_clips`) | Works for all clip types, **but paste only guards generator↔video mismatch** — an audio clip pastes onto a video lane unchecked. |
| Cmd+V dispatch | [input_handler.rs:141-148](../crates/manifold-app/src/input_handler.rs#L141) | Inspector effect-paste first, then `paste_clips`. Never looks at the OS pasteboard. |
| Video source swap | [clip.rs:338-348](../crates/manifold-editing/src/commands/clip.rs#L338) (`SwapVideoCommand`) | The in-repo precedent for P4's audio replace. **No audio equivalent exists.** |
| Detect-and-Group lane reuse | [percussion_orchestrator.rs:576-616](../crates/manifold-playback/src/percussion_orchestrator.rs#L576) | Group found by `Layer.detect_group_source` (correct, survives rename). **Stem lanes found by NAME match** `"{base} · {Stem}"` — breaks the moment `base` (the song filename) changes. |
| Stem order | [percussion_orchestrator.rs:524](../crates/manifold-playback/src/percussion_orchestrator.rs#L524) `STEM_DISPLAY = ["Drums", "Bass", "Other", "Vocals"]` | Index order is the pipeline's stem order. P5's role enum mirrors it exactly. |
| Send per stem lane | [percussion_orchestrator.rs:662-672](../crates/manifold-playback/src/percussion_orchestrator.rs#L662) | Reused iff the lane already owns a send (`send_for_layer`). Correct once lanes themselves are reused. |
| Generated-clip provenance | [percussion_orchestrator.rs:800-853](../crates/manifold-playback/src/percussion_orchestrator.rs#L800) (`clear_clip_triggers`) | Deletes ALL clips (triggers + stems) tagged `detection_source == clip`, one undo step. P4 reuses this walk. |
| Send rename | [commands/audio_setup.rs:112](../crates/manifold-editing/src/commands/audio_setup.rs#L112) (`RenameAudioSendCommand`) | Exists; P5 uses it, does not reinvent it. |
| Layer fields | [layer.rs:49](../crates/manifold-core/src/layer.rs#L49) `parent_layer_id`, [layer.rs:89](../crates/manifold-core/src/layer.rs#L89) `detect_group_source` | P5 adds `detect_stem_role` beside `detect_group_source`. |
| objc2 in manifold-app | [manifold-app/Cargo.toml:52-53](../crates/manifold-app/Cargo.toml#L52) | `objc2 0.6` + `objc2-foundation 0.3` already present. `objc2-app-kit 0.3.2` already in Cargo.lock (winit's transitive dep) — adding it to manifold-app pins no new version. |

## 2. Decisions

- **D1 — Pointer position during drags comes from polling AppKit, not from winit.**
  While a file-hover is active, poll the pointer once per frame via the native window
  (`mouseLocationOutsideOfEventStream` on the `NSWindow`, reached from the winit
  raw-window-handle's `NSView`), convert to physical pixels with the backing scale
  factor. Rejected: *patching or forking winit* — a fork held across upgrades for one
  method is worse than a 20-line poll. Rejected: *injecting a `draggingUpdated:` method
  into winit's delegate class at runtime* — fragile against winit internals, and the
  poll gives the same data. Rejected: *waiting for upstream* — the winit issue is
  years old.
- **D2 — One tracker, all file types.** A `DragHoverTracker` owns the hover state
  (entered on `HoveredFile`, cleared on `DroppedFile`/`HoveredFileCancelled`) and the
  polled position. Every drop path (audio, MIDI, image) reads
  `tracker.drop_position().unwrap_or(self.cursor_pos)` — the image path's identical
  stale-cursor bug is fixed for free, with no per-path logic.
- **D3 — The drop target is always visible during the drag.** Audio/MIDI file over an
  audio lane: that lane highlights and a beat-position line shows where the clip
  starts. Over anything else: a floating label near the pointer reads "New lane:
  ⟨filename⟩". The user must never have to guess which of the two outcomes a drop
  produces — that guess is the current UX failure.
- **D4 — Finder-paste vs internal-paste is arbitrated by `NSPasteboard.changeCount`.**
  When the app copies clips internally, snapshot the general pasteboard's
  `changeCount`. On Cmd+V: if the pasteboard holds file URLs AND its `changeCount`
  differs from the snapshot (or the internal clipboard is empty), the Finder copy is
  more recent — ingest the files. Otherwise paste the internal clipboard. Rejected:
  *internal always wins* — a stale internal clipboard would block file paste forever.
  Rejected: *external always wins* — copying a file once would hijack every later
  in-app paste. `changeCount` is AppKit's own recency oracle; use it.
- **D5 — Pasted files land at the playhead on the active lane.** Route through the
  existing `process_dropped_files` with `drop_beat` = current beat and
  `join_audio_layer` = the active layer if it is audio. Ableton's semantics: paste at
  the insert marker on the selected track. v1 handles audio + MIDI (what
  `process_dropped_files` already ingests); video/image paste is Deferred.
- **D6 — Replace is a first-class clip command, not delete-and-redrop.**
  `ReplaceAudioFileCommand` shaped like `SwapVideoCommand` (clip.rs:338): swaps
  `audio_file_path` + `source_duration`, resets `in_point` to 0, clears
  `recorded_bpm` (the old song's BPM is a lie about the new file), **keeps**
  `start_beat`, `duration_beats`, and the detection **config** (sensitivities,
  routing, quantize — the user's tuning), **clears** the cached detection analysis
  and counts (they describe the old audio). The replace composite also deletes every
  generated clip tagged `detection_source == clip` (same walk as
  `clear_clip_triggers`) — stale triggers for a song that no longer plays are worse
  than none. One undo step restores everything. Detection stays manual per the parent
  doc's locked decision — replace never auto-runs Detect.
- **D7 — The replace gesture is the inspector Source row.** The audio section's
  filename row becomes a button → file dialog → `ReplaceAudioFileCommand`. Rejected
  for v1: *drop-onto-clip-replaces* — ambiguous with "add a second clip to this lane
  at this beat," which P1 just made reliable; revisit only if Peter asks after living
  with the inspector gesture (see Deferred).
- **D8 — Stem lanes are identified by role, not name.** New field on `Layer`:
  `detect_stem_role: Option<DetectStemRole>` where `enum DetectStemRole { Drums,
  Bass, Other, Vocals }` (manifold-core, order mirrors `STEM_DISPLAY`, camelCase
  serde, `skip_serializing_if = "Option::is_none"`). Reuse lookup: child of the group
  with the matching role; **fallback to today's name match** for pre-role projects,
  stamping the role on first touch — no load migration pass. On reuse under a new
  song name, rename the lane, its send (via `RenameAudioSendCommand`), and the group
  to the new base — but only where the current name still equals the previous
  auto-generated pattern, so a hand-renamed lane or group is never clobbered.
- **D9 — Audio-onto-video paste is a skip, symmetric with the existing gen/video
  guard.** `paste_clips` gets an audio arm: an audio clip entry pastes only onto an
  existing audio layer, else `skipped += 1`. Rejected: auto-creating an audio lane at
  the paste index — `paste_clips` never creates typed lanes today and P3 doesn't
  change its contract.

**The plausible-wrong architecture, forbidden by name:** you will want to fix P1 by
upgrading winit or vendoring a patched fork — no, poll AppKit from our side. You will
want the drag position in shared state so the content thread can see it — no, every
piece of this design lives on the UI thread; the content thread learns about drops the
same way it does today (commands). You will want `HoveredFile`'s one-shot position…
it has none; the poll is the position source, there is nothing to cache at hover time.

## 3. The one real technical risk — pointer polling during a drag session

`mouseLocationOutsideOfEventStream` is AppKit's documented "pointer position when
you're not receiving events" API, which is exactly the drag situation. But **I could
not verify from this session that it updates live during an external NSDragging
session** — that needs a human dragging a file.

⚠ VERIFY-AT-IMPL (P1, before anything else in the phase): wire the poll behind a
temporary `eprintln!` on `HoveredFile`-active frames, drag a file from Finder across
the window, read the log. If the position is live → proceed. If it is frozen →
fallback is `NSEvent.mouseLocation` (class method, screen coordinates, reads the
hardware pointer) converted via `convertPointFromScreen`; verify the same way. If
BOTH are frozen during drags, stop and escalate — the design's P1 premise fails and
Peter decides between a winit fork or living with drop-at-last-cursor.

**VERIFIED 2026-07-05 (Peter, live drag test) — BOTH POLL SOURCES FROZEN; D1 IS DEAD.**
Poll wired behind the `eprintln!`; Peter dragged an audio file while moving the pointer
across the window. `mouseLocationOutsideOfEventStream`: byte-identical every frame
(frozen at the pre-drag point). Fallback `+[NSEvent mouseLocation]` + `convertPointFromScreen`:
also byte-identical every frame. The poll site (`about_to_wait`) DOES run during the
NSDragging session — the log streams — so the event loop isn't starved; the position
APIs simply don't update while macOS owns the drag. **Polling cannot work.**

Root cause + the real fix (supersedes D1): during an NSDragging session the only live
pointer is what AppKit hands the destination view via `draggingUpdated:`
(`[sender draggingLocation]`, window coords, live). winit already registers its content
view as a drag destination — that's how `HoveredFile`/`DroppedFile` arrive — but throws
the location away. The fix is to intercept `draggingUpdated:` on winit's view (subclass or
swizzle) and stash the live location for the drop arms. D1 rejected exactly this as
"fragile against winit internals"; the polling bet it chose has now failed both sources,
so D1 no longer holds.

**Decision (Peter, 2026-07-05): P1 + P2 PARKED this pass — P3/P4/P5 land without them.**
A dedicated **Fable** session will design the `draggingUpdated:` interception (or a winit
fork) against winit 0.30.13's macOS `window_delegate.rs`. The polling prototype on
`lane/ingest-p1` (a `drag_hover.rs` tracker + per-frame poll) was discarded — the new
mechanism restructures it — but two results it proved carry forward: the drop-site pattern
is `drop_position().unwrap_or(cursor_pos)` at the three `DroppedFile` arms (audio/MIDI,
image, glTF), and the target coordinate convention is **logical, top-left origin** (winit
stores `cursor_pos` post-`logical_cursor`, so any live source must flip AppKit's
bottom-left view point by view-height). Tracked as BUG-028.

**BUILT 2026-07-05 (Sonnet, `wave/timeline-drop`, pending Peter's live-drag gate before
landing).** Fable's investigation (same session, brief at
`.claude/briefs/TIMELINE_INGEST_P1P2_DROP_BRIEF.md`) found the actual interception point:
winit's macOS drag destination is the `NSWindow`'s **window delegate**, not a view, and
that delegate implements `draggingEntered:`/`performDragOperation:`/etc. but NOT
`draggingUpdated:`. So the fix is a fresh `class_addMethod(draggingUpdated:)` on the
delegate's class (returns `NSDragOperationCopy`, no swizzle needed since the method didn't
exist) plus a swizzle of the existing `performDragOperation:` (captures the drop position
even if the pointer never moves again after entry) — both writing
`[sender draggingLocation]`, converted window-point → view-point
(`convertPoint:fromView:nil`) → flipped to `cursor_pos`'s logical top-left convention, into
a UI-thread-only cell. See `crates/manifold-app/src/drag_interpose.rs` for the full
mechanism and its doc comment (which also states the one assumption a live drag alone can
prove: that `NSWindow` forwards a dragging message to a delegate that only gained the
method at runtime). `crates/manifold-app/src/drag_hover.rs`'s `DragHoverTracker` wraps it
per D2; all three `DroppedFile` arms read `drop_position().unwrap_or(cursor_pos)` per the
pattern above. P2 shipped as a full-length translucent ghost clip on the target audio lane
(`app_render.rs`, reusing the `ClipBody`/`emit_clips`/ghost-alpha pipeline in-app clip-move
drags already use) — the D3 "New lane: ⟨filename⟩" label and a discrete beat-line were
**not** built (no existing floating-text-over-viewport primitive to reuse; scoped out of
this pass, not silently dropped). Compiles clean, clippy clean, full `manifold-app` test
suite green, 4 new unit tests for the coordinate flip. Tracked as BUG-028 (updated).

## 4. New pieces (committed shapes)

```rust
// manifold-app/src/drag_hover.rs — UI thread only, no shared state.
pub struct DragHoverTracker {
    hovered_files: Vec<std::path::PathBuf>,   // accumulated HoveredFile events
    pointer_px: Option<Vec2>,                 // physical px, window space; None until first poll
}
impl DragHoverTracker {
    pub fn on_hovered_file(&mut self, path: PathBuf);
    pub fn on_drag_ended(&mut self);          // DroppedFile-consumed or HoveredFileCancelled
    pub fn poll(&mut self, window: &winit::window::Window);  // no-op unless hovering
    pub fn is_active(&self) -> bool;
    pub fn drop_position(&self) -> Option<Vec2>;
    pub fn hovered_files(&self) -> &[PathBuf];
}
```

```rust
// manifold-app/src/macos_pasteboard.rs
pub fn general_change_count() -> i64;                     // NSPasteboard.generalPasteboard.changeCount
pub fn file_urls_on_general_pasteboard() -> Vec<PathBuf>; // readObjects(NSURL, fileURLsOnly)
```

```rust
// manifold-editing/src/commands/clip.rs — beside SwapVideoCommand, same undo shape.
pub struct ReplaceAudioFileCommand {
    clip_id: ClipId,
    old_path: String,            new_path: String,
    old_source_duration: Seconds, new_source_duration: Seconds,
    old_in_point: Seconds,       // new is Seconds::ZERO
    old_recorded_bpm: f32,       // new is unset
    old_detection: Option<AudioClipDetection>,  // new = old config, analysis/counts cleared
}
```

```rust
// manifold-core/src/layer.rs — beside detect_group_source (layer.rs:89).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DetectStemRole { Drums, Bass, Other, Vocals }   // order = STEM_DISPLAY

#[serde(default, skip_serializing_if = "Option::is_none")]
pub detect_stem_role: Option<DetectStemRole>,
```

Dependency grant (P1 + P3 only): `objc2-app-kit = { version = "0.3", features =
["NSWindow", "NSView", "NSPasteboard", "NSPasteboardItem", "NSEvent"] }` in
manifold-app, plus `NSURL`/`NSArray` features on its existing objc2-foundation dep.
Already in Cargo.lock via winit; no new version enters the tree. Feature list is
⚠ VERIFY-AT-IMPL (compiler will name any missing feature — add the named one, nothing
speculative).

## 5. Phasing

### P1 — True drop targeting *(app)*
- **Entry state:** clean main; `rg -n "self.cursor_pos" crates/manifold-app/src/app.rs` shows the DroppedFile/image arms reading it (re-verify the audit anchors app.rs:2388-2412, project_io.rs:605-694).
- **Read-back:** this doc §2 D1/D2, §3, §4; `app.rs` WindowEvent arms for `HoveredFile`/`DroppedFile`/`HoveredFileCancelled`; the §3 verify step runs FIRST.
- **Deliverables:** `drag_hover.rs` + tracker wired into the event loop (poll called in the per-frame tick while active); every drop arm (audio/MIDI at app.rs:2392, images at app.rs:2429) resolves position via `tracker.drop_position().unwrap_or(self.cursor_pos)`; objc2-app-kit dep added.
- **Gate:** positive — Peter drags a file from Finder onto an existing audio lane and the log reads `Dropped 1 audio file(s) onto lane`; onto empty space reads `as new lane(s)`; an image drop lands on the lane under the pointer. Unit tests for the screen→window→px conversion (`cargo test -p manifold-app drag_hover` — manifold-app is bin-only, so no `--lib` target exists). Negative — the DroppedFile arm no longer reads `self.cursor_pos` directly: `rg -n "cursor_pos" crates/manifold-app/src/app.rs` hits only the tracker fallback and unrelated arms.
- **Forbidden moves:** patching/forking winit · runtime method injection into winit classes · any `Arc<Mutex>`/cross-thread position sharing · caching one position at hover-enter instead of polling.
- **Test scope:** `cargo test -p manifold-app` (bin-only crate — no `--lib` target) + clippy. No parity, no workspace sweep (no renderer/core surface).

### P2 — Drop-target highlight *(ui, app)*
- **Entry state:** P1 shipped (tracker exists, `is_active()` true during a Finder drag).
- **Read-back:** D3; how the viewport draws the clip-drag preview today (find it: `rg -n "drag" crates/manifold-ui/src/panels/viewport/ -l`) — shape the highlight like the existing drag feedback, don't invent a parallel overlay system.
- **Deliverables:** while the tracker is active with an audio/MIDI file: target audio lane highlight + beat line at the would-be start; otherwise the "New lane: ⟨filename⟩" pointer label. Cleared on drop/cancel.
- **Gate:** Peter eyeballs the three cases (over audio lane / over non-audio lane / over empty space) in the running app. Headless can't drive an NSDragging session; this gate is manual by nature and says so.
- **Forbidden moves:** a new overlay/render path when the existing drag-preview infra fits · leaving the highlight painted after `HoveredFileCancelled`.
- **Test scope:** `cargo test -p manifold-ui --lib` + clippy.

### P3 — Finder paste + paste type-guard *(app, editing)*
- **Entry state:** re-verify input_handler.rs:141-148 and service.rs:417-471 anchors.
- **Read-back:** D4/D5/D9; the Cmd+V dispatch order in `input_handler.rs`.
- **Deliverables:** `macos_pasteboard.rs`; changeCount snapshot recorded at internal `copy_clips` time; Cmd+V arbitration per D4 routing files through `process_dropped_files` (beat = playhead, join = active audio lane); audio arm in `paste_clips` per D9 with a test beside the existing mismatch test.
- **Gate:** positive — unit test for the arbitration decision table (internal-empty/external-file, internal-fresh/external-stale, both-present-external-newer, text-only-pasteboard); `cargo test -p manifold-editing --lib` for the paste guard; manual — copy a .wav in Finder, Cmd+V, clip appears at playhead. Negative — `rg -n "NSPasteboard" crates/manifold-app/src | rg -v macos_pasteboard` returns zero hits (one module owns the pasteboard).
- **Forbidden moves:** parsing pasteboard *text* as paths (file URLs only) · "external always wins" / "internal always wins" shortcuts · opening a file dialog as the paste implementation.
- **Test scope:** `cargo test -p manifold-editing --lib` + `-p manifold-app` (bin-only — no `--lib`) + clippy.

### P4 — Replace audio file *(editing, app, ui)*
- **Entry state:** re-verify clip.rs:338 (SwapVideoCommand) and orchestrator clear walk (percussion_orchestrator.rs:800-853).
- **Read-back:** D6/D7; `SwapVideoCommand` end-to-end including its undo; the parent doc's locked "Detect is manual" decision.
- **Deliverables:** `ReplaceAudioFileCommand` (§4 shape) + composite with the `detection_source` clip deletions; inspector Source row → file dialog → command (PanelAction precedent: the existing `ClipDetect*` actions in [inspector.rs:730-806](../crates/manifold-app/src/ui_bridge/inspector.rs#L730)); roundtrip + undo test in `manifold-editing/tests/command_roundtrips.rs` (existing file, existing pattern).
- **Gate:** positive — roundtrip test proves undo restores path, in_point, recorded_bpm, detection state, and the deleted generated clips; manual — replace a detected song's file, old triggers/stems vanish, config + routing survive in the inspector, Detect re-populates onto the same lanes. Negative — `rg -n "detect" crates/manifold-editing/src/commands/clip.rs` shows the command clears analysis, never invokes detection.
- **Forbidden moves:** flag-parameterizing `SwapVideoCommand` to double as audio · auto-running Detect on replace · leaving the old song's triggers alive "until next detect".
- **Test scope:** `cargo test -p manifold-editing` (lib + command_roundtrips) + clippy.

### P5 — Role-keyed stem lanes *(core, playback)*
- **Entry state:** re-verify the name-match lookup at percussion_orchestrator.rs:604-616 and `STEM_DISPLAY` order at :524.
- **Read-back:** D8; parent doc §8.3 (the reuse contract this makes hold); the orchestrator tests at the bottom of percussion_orchestrator.rs (`clear_clip_triggers_removes_only_tagged`, `replan_clip_places_from_cache_without_backend`) — new tests follow their harness.
- **Deliverables:** `DetectStemRole` + `Layer.detect_stem_role` (§4); lookup by role with name-match fallback that stamps the role; rename-on-reuse of lane + send + group per D8's don't-clobber rule; test: detect on a lane, replace with a differently-named song (P4), re-detect → assert same lane IDs, same send IDs, zero new lanes, names updated.
- **Gate:** positive — the new orchestrator tests + `cargo test -p manifold-playback --lib` + `-p manifold-core --lib` (serde roundtrip of the new field). Negative — `rg -n 'l.name == lane_name' crates/manifold-playback/src/percussion_orchestrator.rs` returns zero hits as the primary lookup (the name match survives only inside the stamped fallback, commented as such).
- **Forbidden moves:** delete-and-recreate lanes as "rename" · an eager load-migration pass over existing projects (the lazy fallback IS the migration) · renaming user-edited names.
- **Test scope:** focused per above + clippy. No workspace sweep — no GPU/parity surface anywhere in this wave.

Order: P1 → P2; P3, P4, P5 are independent of each other (P5's test wants P4 but can
simulate replace by mutating the path in-test if P4 hasn't landed).

## 6. Decided — do not reopen

1. Pointer via AppKit polling; winit is not patched, forked, or upgraded for this.
2. One tracker serves all file types; image drops fixed via the same accessor.
3. Drop target always visualized (lane highlight / "New lane" label).
4. Finder-vs-internal paste arbitrated by `changeCount`; neither side "always wins".
5. Pasted files land at playhead on the active lane, via `process_dropped_files`.
6. Replace = dedicated command; keeps tuning config, clears analysis + generated
   clips, never auto-detects.
7. Replace gesture is the inspector Source row (v1).
8. Stem lanes keyed by `DetectStemRole`; renames follow the song only where names
   are still auto-generated; lazy role-stamping, no migration pass.
9. `paste_clips` skips audio↔video mismatches, symmetric with gen↔video.

## 7. Deferred

- **Drop-onto-clip = replace** — revive if Peter asks after using the inspector
  gesture; needs a modifier-key convention to disambiguate from add-to-lane.
- **Video/image paste from Finder clipboard** — revive on first request; the D4
  arbitration and routing generalize as-is.
- **Snap the drop/paste beat to the grid** — today's drop is unsnapped; unchanged in
  this wave. Revive when Peter trips over it (one line at the D5 call site:
  `vp.snap_to_grid(...)`).
- **Windows/Vulkan drag position** — this design is AppKit-only by construction;
  the Vulkan-era window layer owns its own equivalent (VULKAN_BACKEND_DESIGN).
- **Multi-file hover preview** (per-file lane assignment while hovering several
  files) — v1 shows the first file's target; revive if multi-file drops become a
  real workflow.
