# Gig Resilience — Red-Teaming the Live Show

**Status: IN PROGRESS — P1 (`3dffe29a` P2 merge) and P2 built + merged into `feat/timeline-ui-redesign` (2026-07-03); P3–P4 not implemented. Sonnet-executable. Phases in §9.**
**D8 status (updated 2026-07-10 — F8):** P2 shipped D8's breadcrumb-beat fallback (autonomous, plays immediately). The 2026-07-03 caveat said the "rejoin to live Ableton position" branch could not be built because the bridge had no inbound song-position field — **that is now false.** `ABLETON_TRANSPORT_SYNC` (P1–P3, landed 2026-07-07) added the inbound listener: `ableton_bridge.rs` parses `/live/song/get/current_song_time` into `PendingTransportState::song_time` (`:242`, `:891`). **Peter's decision (2026-07-10): build the `--resume` Ableton-position rejoin now.** It is remaining work here (wiring `--resume`'s D8 rejoin to the shipped `song_time` listener — a P3/P4-adjacent task; see §5.2 and §9 P4). The old "deferred to ABLETON_SHOW_SYNC, decision pending Peter" pointer is retired — transport landed in ABLETON_TRANSPORT_SYNC, and the rejoin consumes it from here.
**Prerequisites: none for P1–P2. P3 after PERFORM_SURFACE_DESIGN P1 (`docs/DESIGN_BUILD_ORDER.md` §2).**
**P4 priority (Peter, 2026-07-13): CRITICAL — but do NOT start yet; Peter calls the timing.** P4 (peripheral resilience, GPU-fault surfacing, thermal glyph, quarantine, D7 hardening, and the `--resume` Ableton-position rejoin) is show-critical and outranks the rest of the approved-not-built queue when scheduling resumes. P4 has no dependency on P3 and may run before the understudy.
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before starting any phase.**

MANIFOLD is a live instrument. This doc is the operational failure audit of the
whole gig — pre-show, mid-show, recovery — and the design that makes failure
survivable. The governing insight, settled with Peter (2026-07-02): **crash-only
software**. We do not try to survive a panic in-process; we make death-and-rebirth
so fast and so automatic that the audience never learns it happened. Music never
stops (Ableton keeps playing through any MANIFOLD crash); the job is to keep
pixels on the wall and rejoin the running show without human hands.

Companion designs: `docs/ABLETON_SHOW_SYNC_DESIGN.md` (the score that makes
rejoin exact), `docs/SESSION_MODE_DESIGN.md` (the second performance surface),
`docs/PERFORM_SURFACE_DESIGN.md` (perform mode becomes chrome-hosted — P3's
visible arming indicators are widgets on that surface; sequencing in
`docs/DESIGN_BUILD_ORDER.md` §2).

---

## 1. Audit — what exists today

Verified against the codebase 2026-07-02. File references are load-bearing:
re-verify at implementation time.

### Already solid (shipped hardening)

| Concern | Where | Behavior |
|---|---|---|
| Panic logging | `manifold-app/src/main.rs:67` | Panic hook → `~/Library/Logs/com.latentspace.manifold/crash.log` with backtrace. Installed before anything else. |
| Process hygiene | `main.rs` (10.x block) | SIGPIPE ignored, single-instance file lock, IOPMAssertion (no display sleep), App Nap suppressed. |
| Ableton bridge | `manifold-playback/src/ableton_bridge.rs:564,675,702` | Recv thread wrapped in catch_unwind; thread death → automatic teardown + reconnect; connection timeout → marked disconnected; playback freewheels at last tempo. **The template for every other peripheral.** |
| Display disconnect | `manifold-app/src/app.rs:315-320,2604` | Pending-flag clear + safety net when a display link never fires (display unplugged); EDR surface re-reads display properties on change. |
| Output continuity on stall | `manifold-app/src/shared_texture.rs` | IOSurface triple buffer: if content stops publishing, the UI keeps presenting `front_index` — output holds last frame by construction. |
| Save history | `manifold-io/src/archive.rs:45,143,341` | Every V2 save pushes the previous state into `history/` inside the archive, with manifest entries and auto-save pruning (all manual saves kept, autosaves capped). |
| Nothing-scheduled blackout | `manifold-app/src/content_thread.rs:752` | Empty timeline region renders deliberate black, not garbage. |

### Gaps (the red-team findings)

| # | Finding | Evidence | Consequence on stage |
|---|---|---|---|
| G1 | `panic = "abort"` in release — any panic on any thread kills the whole process. Also silently disables every `catch_unwind` (the bridge's isolation is dev-only). | `Cargo.toml:113` | The show-ending event is "process dies," not "thread dies." Decided: this is CORRECT (§2 D1) — but it makes G2/G3 mandatory, and worker threads must stop relying on catch_unwind (D7). |
| G2 | Autosave machinery exists but is never triggered — every `save_project` call passes `is_auto=false`; no timer anywhere. | `manifold-app/src/app_lifecycle.rs:58`, `project_io.rs:348,404` | Crash = everything since last Cmd+S gone. |
| G3 | No relaunch-into-show path. Reopen, transport, bridge reconnect, output window placement: all manual. | — | Crash-to-picture measured in minutes, in the dark, mid-set. |
| G4 | Save and load failures are log-only. No dialog, no toast. | `project_io.rs:323,358` | Disk full at soundcheck → you believe you saved; you didn't. |
| G5 | MIDI: scan at `start()` + device-filter change only. No hotplug rescan, no reconnect. | `manifold-playback/src/midi_input.rs:216,238` | Controller cable knocked out → dead until app restart. |
| G6 | No Cmd+Q guard, no unsaved-changes prompt anywhere. | (no `unsaved`/confirm hits in `manifold-app`) | One wrong keystroke ends the show instantly. |
| G7 | Audio capture: stream error → `log::error` only. No rebuild, no default-device-change follow. | `manifold-audio/src/capture/cpal_input.rs:127` | Interface unplugged → audio-reactive layers go statue, silently. |
| G8 | Content-thread hang (GPU stall, graph cycle) is invisible: UI stays live, output holds last frame, nothing detects it. | — | Frozen show with a healthy-looking laptop. |
| G9 | GPU command-buffer error status checked only on the compositor encoder; no recovery anywhere. | `manifold-app/src/content_pipeline.rs` (single `add_completed_handler_with_status`) | AGX faults (see `agx-setvertextexture-0x78-crash` memory) surface as mystery freezes. |
| G10 | `crash.log` is a single file, overwritten each crash; never surfaced in-app. | `main.rs:85` (`fs::write`) | Second crash destroys evidence of the first; nobody reads it. |
| G11 | Zero thermal awareness. | (no hits) | Hot stage → throttle → frame drops with no explanation. |
| G12 | Output window topology (which display, fullscreen state) not restored on relaunch. | `window_registry.rs` (runtime only) | Even a fast relaunch lands windowed on the laptop screen. |

---

## 2. Decisions (settled with Peter 2026-07-02 — don't reopen)

- **D1 — Crash-only. `panic = "abort"` stays.** A panic means an invariant broke;
  the content thread owns the `Project`, so post-panic in-memory state is never
  trustworthy. The only known-good state is the last save on disk — so real
  recovery is always relaunch-and-reload. Abort makes that explicit and removes
  the temptation to limp on poisoned state. **Never restart the content thread
  in-process.**
- **D2 — The watchdog ("understudy") is REQUIRED, not optional.** A separate
  tiny process that covers the output display the instant the main app dies or
  hangs, plays fallback content, relaunches MANIFOLD behind itself, and hands
  the display back on first good frame. Spec in §4. Peter: "the watchdog idea
  is fantastic."
- **D3 — Every rung of the recovery ladder is autonomous.** Peter: "driving
  manually does not work, it defeats the purpose." No rung may require human
  input mid-set. The floor is not a dialog — it is the watchdog playing the
  fallback loop for the rest of the set.
- **D4 — Autosave = versioned full snapshots via the existing `history/`
  mechanism.** No diff format — projects compress to a few MB; diffs are
  complexity for nothing. Peter asked for versioned revert: the archive already
  stores it; what's missing is the trigger and the browser UI (§6).
- **D5 — Show mode is not a separate toggle: entering perform mode arms the
  protections** (watchdog spawn, Cmd+Q guard, set-start snapshot, autosave
  timer parks). Exiting disarms. Editor mode never runs the watchdog — there,
  a crash is an ordinary crash and autosave protects the work.
- **D6 — Crash-to-picture: "as fast as possible within reason."** Two-tier:
  fallback pixels within ~100 ms (watchdog cover), full show back in low
  single-digit seconds (relaunch + load + rejoin). No hard SLA; every phase
  should shave it.
- **D7 — Worker threads must not panic, by construction.** Under abort,
  catch_unwind is dead code in release. The bridge recv thread, audio analysis
  worker, and background save thread get Result-hardened (no unwrap/expect on
  fallible paths; errors → log + degrade). The content thread is exempt: its
  panics are *supposed* to kill the process (D1).
- **D8 — Rejoin authority: Ableton transport when connected, breadcrumb beat
  otherwise.** When synced, position comes from the still-running Ableton set
  (the bridge already tracks song time + play state — `ableton_bridge.rs`
  `PendingTransportState`); rejoin is exact because the show-sync score is
  beat-anchored. Standalone: resume playing from the last breadcrumb beat.
- **D9 — Deterministic-crash quarantine targets the *next* occurrence, not the
  current one.** Because rejoin follows live transport, the crash beat is
  already behind you when you're back — the risk is the same content re-firing
  at the next section repeat. Quarantine = disable the suspect content for the
  rest of the show (§5.3).
- **D10 — Cover content: black.** Peter: "just black is fine if it's going to
  recover within a few seconds." A few seconds of black reads as a lighting
  choice; recovery makes it a blink. Optional per-project fallback loop
  (AVPlayerLayer) exists for the rung-3 case — if relaunch keeps failing, a
  loop for the rest of the set beats a black wall — but it is opt-in polish,
  not required setup. Either way the understudy carries zero manifold-gpu
  code, so it cannot share a failure mode with the thing it's covering for.

---

## 3. Failure-mode catalog (ranked)

**Show-ending** (audience sees it, no recovery without this design):
process panic (G1) · process hang (G8) · accidental Cmd+Q (G6) · relaunch
lands wrong (G3, G12).

**Show-degrading** (performance continues, capability lost):
MIDI controller loss (G5) · audio capture loss (G7) · Ableton link loss
(already handled — freewheel + auto-reconnect) · display unplug (partially
handled) · GPU fault on one encoder (G9) · thermal throttle (G11).

**Work-destroying** (nobody sees it until later):
no autosave (G2) · silent save failure (G4) · crash.log overwrite (G10).

The design attacks them in that order: §4–5 kill the show-enders, §6 the
work-destroyers, §7–8 the degraders.

---

## 4. The watchdog — `manifold-understudy`

A separate minimal binary (own crate, `crates/manifold-understudy`), spawned by
the main app on entering perform mode, killed on exit.

**Independence rule:** the understudy links AppKit + AVFoundation only. No
manifold-gpu, no manifold-core, no shared code that could carry the same bug
that killed the main app. It must be boring enough to trust.

### 4.1 What it does

1. **Cover.** Owns a borderless window per output display at
   `NSScreenSaverWindowLevel`, initially hidden (alpha 0 / ordered out). On
   trigger, orders it front instantly: black (D10; optional loop asset if the
   project sets one).
2. **Watch.** Two signals:
   - *Death:* it spawned the app process (or was handed its PID) — `SIGCHLD` /
     kqueue `EVFILT_PROC` fires on exit. Covers panic-abort, Cmd+Q that slipped
     through, the OS killing us.
   - *Hang:* heartbeat over a unix domain socket. The **content thread** (not
     the UI thread — it's the one that matters) writes a heartbeat every tick.
     Stall > 2 s while transport is playing → treat as hang: cover, `SIGKILL`,
     ladder. This unifies panic and hang into one recovery path (kills G8).
3. **Relaunch.** `manifold --resume <breadcrumb-path>` per the ladder (§5).
4. **Hand back.** Main app signals "first output frame presented" on the same
   socket → understudy fades its cover over ~300 ms and rearms.

### 4.2 Ladder governor

The understudy owns the crash counter (per show — reset when the main app exits
cleanly or the operator disarms perform mode):

| Crash # this show | Action |
|---|---|
| 1 | Cover → relaunch `--resume` → hand back. Transient crashes (races, driver hiccups) are the common case; this covers them in seconds. |
| 2 | Same, plus pass `--quarantine <crash-report-path>` — main app disables suspect content on load (§5.3). Still fully autonomous. |
| 3+ | Stop relaunching. Cover stays up (black, or the loop if set) for the rest of the set. The operator decides at the next natural break — never a dialog on the output. |

### 4.3 Protocol notes

- Socket protocol is 3 message types (`HEARTBEAT beat=<f64>`, `PRESENTED`,
  `DISARM`) — plain text, versioned by a hello line. Keep it OS-neutral: the
  Vulkan-era Windows/Linux understudy reimplements the window + player layer,
  not the protocol.
- The understudy never draws attention on the *control* display — status goes
  to a small strip the main app renders when connected, plus its own log file.
- Testing: `kill -9` drills and `SIGSTOP` (hang simulation) against a running
  perform session are the acceptance tests. See §9.

---

## 5. Resume path — `manifold --resume`

### 5.1 Breadcrumb sidecar

The content thread writes a tiny state file (atomic tmp+rename, same dir as the
project archive: `<project>.manifold.breadcrumb`):

- project path, show/perform-mode flag
- current beat + wall clock + transport playing flag
- active generator/effect `type_id`s and the clip IDs scheduled *right now*
- output window topology: display UUIDs, fullscreen state, per-display bounds
  (closes G12)

Cadence: on integer-beat change while playing, plus on any transport change —
never per frame, never on the hot path (a few writes per second, background
thread, pre-serialized buffer). The panic hook also appends current beat (from
an atomic the content thread keeps fresh) to the crash log, so crash reports
are score-addressed.

### 5.2 Boot fast path

`--resume` skips everything that isn't pixels:

1. Parse breadcrumb → open project (normal open already yields last-saved
   state, which includes autosaves — §6).
2. Restore output windows from breadcrumb topology (display UUID match;
   missing display → fall back to largest non-primary, then primary).
3. Enter perform mode (which re-arms protections, D5), connect Ableton bridge.
4. Rejoin transport per D8: Ableton position if connected within a short
   window (~2 s), else breadcrumb beat. Start playing without waiting for the
   bridge — re-seek when it connects (the score is beat-anchored; a re-seek is
   a snap, not a scrub — see `timecode-locks-score-not-render` memory).
   **[F8, 2026-07-10 — the Ableton-position branch is now buildable and Peter approved
   building it.** Wire it to the shipped inbound listener: read
   `PendingTransportState::song_time` (`ableton_bridge.rs:242/:891`, added by
   ABLETON_TRANSPORT_SYNC) — if it has updated within the ~2 s window, seek to that beat;
   else breadcrumb beat. This is the remaining `--resume` work, briefed in §9 P4.]**
5. First frame presented → `PRESENTED` to the understudy.

Engineering the number: project load at canonical scale (2 928 clips, 53
layers) is JSON-in-ZIP + texture warmup; MSL binary cache
(`manifold-gpu/src/metal/msl_cache.rs`) already exists. Target low
single-digit seconds; profile in P2 and shave the biggest slice, don't
gold-plate.

### 5.3 Quarantine (second crash, autonomous)

Goal per D9: stop the *next* re-fire. On `--quarantine`:

1. Read the crash log backtrace. Symbol-match against primitive / preset
   module paths (`node_graph/primitives/*`, preset names). Hit → disable every
   clip whose generator/effect chain references that `type_id` (mute the clip,
   keep the layer; the mute is an ordinary undoable edit tagged in the sync
   report style, so post-show cleanup is one selection).
2. No backtrace hit → fall back to the breadcrumb's "scheduled right now" clip
   IDs at crash beat: mute those clips only.
3. Surface what was quarantined on the control display (toast + list), never
   on the output.

This is a heuristic and stays one: v1 does not attempt binary search, layer
bisection, or beat-window inference. If quarantine guesses wrong and crash 3
arrives, the fallback loop is the floor and it is show-safe.

---

## 6. Autosave + versioned revert

- **Trigger:** dirty-debounced — N seconds (default 60) after the last edit,
  only if `EditingService` reports dirty. No blind wall-clock saves.
- **Zero-hitch:** serialize from the UI's `Arc<Project>` snapshot on a
  background thread (the snapshot channel already exists; a save must never
  block the content tick or the UI frame). Write via the existing
  `saver::save_project(…, is_auto=true)` → history entry + pruning for free.
- **Perform mode (D5):** on entry, take one labeled "set start" snapshot; then
  the autosave timer **parks** — no mid-set disk surprises, and mid-set edits
  are performance gestures, not compositions. On exit, one "set end" autosave.
- **Revert UI:** a history browser over the archive's existing `history/`
  entries (timestamp, label, auto/manual) → open-as-copy or restore. This is
  the "versioned diffs" ask (D4): the storage half already ships; this is the
  missing half.
- **Undo-history persistence (Peter, 2026-07-03):** "it would be nice to also
  save the undo and redo history so if a project crashes you can still undo
  what work was done." Two shapes, decide at P1: **(a)** serialize the undo
  stack itself — Cmd+Z/Shift-Cmd+Z work across relaunch, but requires
  `Serialize` across every `Command` type in `manifold-editing` (large
  surface, ~all commands); **(b)** denser `history/` granularity — the
  debounced autosave already journals edit bursts as full snapshots, so the
  history browser gives step-back/step-forward through recent states with
  zero new serde. Recommendation: ship (b) with P1 (it's free — it IS the
  autosave), then assess whether (a)'s true-undo UX justifies the serde pass
  as a follow-on. Don't half-build (a): partial command coverage means an
  undo stack that lies.
- **crash.log rotation (G10):** timestamped files, keep last 20. On next
  *editor-mode* launch after an unclean exit: one quiet banner — "MANIFOLD
  crashed last session — crash log + last autosave available." Never shown on
  a `--resume` boot.

## 7. Perform-mode arming (D5)

On entering perform mode: spawn understudy · Cmd+Q (and Cmd+W on output
windows) require holding the key combo ~2 s with visible progress · "set
start" snapshot · autosave parks · crash counter resets. On exit: reverse.
No new mode, no separate toggle, no preference.

Where this attaches (verified 2026-07-03): the arming hooks ride perform
mode's enter/exit lifecycle (`perform_mode/mod.rs` `pending_enter` /
`pending_exit`), which survives the chrome-hosting rebuild in
`PERFORM_SURFACE_DESIGN` P1 unchanged. The *visible* pieces — hold-progress
indicator, understudy status strip, thermal glyph (§8) — are chrome widgets
on the perform surface, not hand-drawn HUD. Build P3 after PERFORM_SURFACE
P1 (`docs/DESIGN_BUILD_ORDER.md` §2) so they're built once.

## 8. Peripheral resilience (the degraders)

All follow the Ableton-bridge template: detect → log + surface → auto-retry
forever → seamless re-attach. Result-hardened per D7.

- **MIDI (G5):** CoreMIDI `MIDINotifyProc` (or 2 s poll of `ports()` as the
  simple v1) → new matching device appears → register it; known device
  vanishes → drop it and keep watching. Controller re-plug mid-set just works.
- **Audio capture (G7):** stream error callback sets a failed flag → owner
  rebuilds the stream next tick; follow default-device changes for the
  default-input path; output taps revalidate on CoreAudio device-list change
  (`docs/AUDIO_INFRASTRUCTURE.md` §11). Analysis worker already tolerates
  silence; the fix is reattachment, not analysis.
- **GPU faults (G9):** wire `add_completed_handler_with_status` on every
  submitted command buffer in the content pipeline (label = pass name). On
  error status: log with label + increment a HUD-visible fault counter. No
  in-process recovery attempts (D1) — persistent faults escalate to the
  heartbeat/hang path naturally.
- **Thermal (G11):** read `NSProcessInfo.thermalState` on a slow timer;
  surface as a perform-HUD glyph (nominal/fair/serious/critical). Telemetry
  only, v1 — no auto-degradation; the operator sees it coming instead of
  guessing.

## 9. Phasing (Sonnet-executable)

Entry state for every phase: re-verify the §1 anchors the phase touches (the
audit is a 2026-07-02 snapshot). Forbidden across all phases: catch_unwind as a
fix (dead code under `panic = "abort"` — D7 means Result-hardening, not
catching); in-process recovery of any kind (D1); new `Arc<Mutex>`/`Arc<RwLock>`
(baseline the count with `rg -c 'Arc<Mutex|Arc<RwLock' crates/` first — it must
not grow); any rung that needs human input mid-set (D3).

- **P1 — Don't lose work. ✅ BUILT + MERGED (2026-07-03, `feat/timeline-ui-redesign`).**
  New `manifold-app/src/autosave.rs` + `alerts.rs`, history browser, save/load
  error surfacing, crash.log rotation. Automated gate green in the merged workspace
  sweep (`manifold-io/tests/history_snapshots.rs`). **Two manual checks still owed
  by Peter:** (1) autosave end-to-end — edit → wait debounce → File → Revert to
  Snapshot; (2) save to a full/read-only volume surfaces the UI event.
  Autosave wiring + background serialization +
  history browser + save/load error surfacing + crash.log rotation & banner.
  Read-back: §6 whole, `archive.rs:45,143,341`, `app_lifecycle.rs:58`,
  `project_io.rs:323-404`. Forbidden: a new save path — everything routes
  through the existing `saver::save_project(…, is_auto=true)`; serializing on
  the content or UI thread (snapshot → background thread, §6). Gate: test —
  edit, wait debounce, assert history entry appears + prune caps hold; save to
  a full/readonly volume surfaces a UI event (manual check); touches
  `manifold-io` save path = infrastructure → full workspace sweep.
- **P2 — Come back fast. ✅ BUILT + MERGED (2026-07-03, `3dffe29a`).** New
  `manifold-app/src/breadcrumb.rs`: sidecar (atomic write, integer-beat cadence,
  background writer), `--resume` boot path (opens via existing ProjectIOService,
  seeks breadcrumb beat, display-UUID topology restore), panic-hook beat stamp
  (single atomic). Reads beat via the existing `ContentState` channel — `sync_clips_to_time`
  untouched, `Arc<Mutex>` count unchanged. 10/10 breadcrumb unit + resume/crash tests +
  workspace sweep green. **D8 breadcrumb-beat fallback shipped; the Ableton-position
  rejoin branch is now approved and buildable (F8, 2026-07-10 — the inbound listener
  landed in ABLETON_TRANSPORT_SYNC; remaining work is in P4).** Live kill→relaunch crash-to-picture profiling is a
  PETER manual drill (documented in the P2 report; instrumentation at `app_lifecycle.rs:575`
  + a `[Resume]` log line) — not yet run.
  Breadcrumb sidecar + `--resume` boot path + output
  window topology restore + transport rejoin (Ableton-first, breadcrumb
  fallback) + panic-hook beat stamp. Read-back: §5 whole. Forbidden:
  per-frame breadcrumb writes (cadence = integer-beat + transport changes,
  §5.1); blocking IO on the content tick (pre-serialized buffer, background
  write). Gate: kill the app while playing, relaunch `--resume` by hand →
  correct project, correct beat, correct output display; **profile
  crash-to-picture and report the measured number** (target: low single-digit
  seconds at canonical scale — 2 928 clips). No watchdog needed yet.
- **P3 — The understudy.** New `manifold-understudy` crate + socket protocol +
  heartbeat from content tick + cover/relaunch/handback + ladder governor +
  fallback-asset setting + perform-mode arming (spawn/disarm, Cmd+Q hold
  guard — visible indicators are perform-surface widgets, §7). **Named line item
  (coherence audit F19, 2026-07-10): the panic/understudy path must call LED
  `blackout()` (`LED_STRIPS_DESIGN.md` D8) so dead render never freezes strips at
  full white — LED_STRIPS D8 names this doc as the owner but the cross-reference was
  never added here until now; wire it into the cover/relaunch path this phase, not a
  follow-on.** Read-back: §4
  whole, §7, PERFORM_SURFACE_DESIGN §4, `LED_STRIPS_DESIGN.md` D8. Forbidden: ANY manifold-* dependency
  in the understudy (negative gate: `cargo tree -p manifold-understudy` shows
  AppKit/AVFoundation bindings only — the independence rule is the design);
  heartbeat from the UI thread (it must be the content tick, §4.1). Gate =
  the drills, scripted in `scripts/gig_drill.sh`: `kill -9` mid-show →
  fallback pixels <100 ms + full show back autonomously (LED strips included —
  a drill run with LED patched must show blackout, not frozen white); `SIGSTOP` →
  same via hang detection; 3-crash script → cover stays, no relaunch spam. Ladder
  table §4.2 is the test surface.
- **P4 — Peripherals + polish.** MIDI hotplug, audio rebuild/device-follow,
  GPU CB status wiring, thermal glyph, quarantine heuristic (§5.3),
  Result-hardening pass on worker threads (D7), **`--resume` Ableton-position rejoin
  (F8, Peter-approved 2026-07-10): wire §5 step 4's Ableton-first branch to the shipped
  `PendingTransportState::song_time` listener (`ableton_bridge.rs:242/:891`) — seek to the
  live song beat if it updated within the ~2 s window, else breadcrumb beat.** Read-back:
  §8, §5.2, the Ableton-bridge template (`ableton_bridge.rs:564,675,702`) + the inbound
  transport parse (`:891`). Forbidden: quarantine cleverness beyond the two §5.3
  heuristics. Gate: focused per-crate tests + manual unplug/replug drill per peripheral;
  **for the rejoin, a kill→relaunch drill with Ableton playing lands the show at Ableton's
  current beat, not the breadcrumb beat, when the bridge reconnects inside the window;**
  for D7, `rg -n 'unwrap\(\)|expect\(' ` over the three named worker threads' fallible
  paths returns only cases with a stated infallibility reason.

Testing note: P3's drills belong in a script (`scripts/gig_drill.sh`): launch,
enter perform mode, kill/-STOP the process, assert understudy state
transitions from its log. The ladder table in §4.2 is the load-bearing test
surface.

## 10. Ops checklist (no code — the laptop is part of the instrument)

**This is the single named pre-gig checklist (coherence audit F17, 2026-07-10) —
three other docs each add a pre-show ritual with no shared home; this section is now
where they're named so nobody re-derives a fourth list:** the `kill -9` drill below
(this doc, §9 P3) · **the perf soak** — `cargo xtask perf-soak` against the Liveschool
fixture (`PERF_BUDGET_GATE_DESIGN.md` P1–P3 — frame-budget regression check before a
show) · **the recorder soak** (`LIVE_RECORDING_PROOFS_DESIGN.md`
Tier 2 — manual pre-gig recording rehearsal). Run all three at soundcheck, not just this
list's items.

Pre-show, every show: black desktop wallpaper on all displays · Dock +
menu bar auto-hide · Do Not Disturb / Focus on · notifications off ·
auto-updates off · nothing else running · power adapter + "prevent sleep" ·
`caffeinate` unnecessary (IOPMAssertion ships) · one `kill -9` drill at
soundcheck — if you haven't drilled it, you don't have it.

## 11. Deferred (explicitly not v1)

- Auto-degradation on thermal/perf pressure (drop resolution before dropping
  frames) — needs the perf-gate work; telemetry first.
- Understudy on Windows/Linux — protocol is ready; lands with the Vulkan
  platform work (`docs/VULKAN_BACKEND_DESIGN.md` §8).
- Quarantine beyond the two heuristics (bisection, beat-window inference).
- A/B dual-machine failover (two laptops, one show) — different tier of rig;
  the understudy protocol is deliberately not designed for it.
- Crash-loop telemetry upload / remote diagnostics.
