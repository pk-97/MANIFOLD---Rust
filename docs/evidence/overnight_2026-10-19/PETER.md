# For Peter — what only you can do or decide

Written at the end of the 2026-10-19 overnight run, on main at 3caad3144. Five parts: the one thing to read first, the designs waiting on your look or call, the open decision beads, the test-suite cut list, and the list of things to try in the live app. Notes at the end. The night's record is `REPORT.md` next to this file.

## Top: quitting with unsaved changes loses them

BUG-akqgc (Quitting with unsaved changes loses them: no prompt, autosave only 60s after the last edit) is a P1 and it is real show risk. Closing the main window sends Shutdown straight away. Nothing checks whether the project has unsaved edits and nothing asks you.

The only safety net is autosave. It fires 60 seconds after the last edit, only for a project that already has a file path, and only in editor mode. So quitting within a minute of an edit drops that edit. Quitting an untitled project drops everything. Quitting from perform mode drops everything since your last manual save.

The fix is a Save / Don't Save / Cancel prompt on close, or one last save before Shutdown. It has to cope with a running export, where saving is refused. It is not built yet. Until it is, save by hand before you quit.

## Designs waiting only on your look or call

The board (`.claude/hooks/design_status.py`) lists these. One line each.

- ROLL_DETECTOR: proposed, awaiting your answers to the open decisions in its section 15.
- MERGE_MODIFIER: proposed, not built; your edge-case rulings are folded in, the build waits on a go.
- SHOW_PREPARATION: an architecture draft from Codex, not approved for implementation.
- SCENE_SPLAT_MODIFIERS: proposed, not implemented, waiting on a go.
- SCENE_DECIMATE: proposed; the architecture needs a revision (its section 8) before anyone builds it.
- LIQUID_SOLVER_SEAM: proposed; your calls are in its section 8, and several landed phases owe gates.
- SCENE_MIRROR: the framework it targeted is gone; it waits on your A/B call, then redesign or land.
- SCENE_MODIFIER_RT: built; waits on your review of the qualification gaps.
- PROJECT_FOLDERS: P1 to P5 landed; waits on your live check.
- FRAME_EXACT_TRANSPORT: P1 to P3 on main; waits on your rig test of the Link, MIDI clock and timecode follower, then P4 (BUG-l0pnn (Frame-exact transport: rig test, constant tuning, P4 video locking)).
- BLOB_TRACKING_V2: implemented; owes your look at mask refinement (BUG-sh4i (Blob mask visual refinement)).

## Open decision beads

`bd list --type decision --status open` shows 35. One line each.

- The rule-sources redline is child 19 of BUG-zezjf (CLAUDE.md, AGENT_ROUTING and memory redline: approve or amend): the rule-sources collapse waits on your yes.
- BUG-yqce (scene_mirror re-home after kind-framework removal): where the mirror modifier lives now.
- BUG-gptz9 (Design pass: the standard D/E/A modulation buttons on scene switches and pick lists): a task, but it hangs on the next bead.
- BUG-n39yw (Should effect and generator cards outside scenes draw labelled params as pick lists and switches): yes or no.
- BUG-8fsrs (MANIFOLD.app launcher: delete or keep).
- BUG-lsj7c (Automation lane mute: build the toggle or delete the flag): the flag works in playback, nothing in the UI sets it.
- BUG-n7gtm (Percussion placement on a video clip: feature or drop the fallback).
- BUG-u3ov (MPM solver budget, 6 or 12 ms): decide at P4 from the numbers.
- BUG-5vnr (jitter-seam-visual-seamlessness-call): a look call.
- BUG-8txd (Live matter GPU time above res about 100): cap the card, add a budget, or leave it.
- BUG-mp3z (GPU liquid surface look): smooth and glassy against the neighbour-search cost.
- BUG-kjxf (water: perf optimization direction after the S8 42ms/frame miss): Astra and you.
- BUG-01vr (Water dt-halving gate): trajectory divergence evidence needs an Astra ruling.
- BUG-6kq7 (Sim tick region: four calls before build).
- BUG-spt5p (How long should warmup wait for one heavy layer before giving up).
- BUG-7phe7 (Stepped envelopes on session-detached layers after stop/seek).
- BUG-yq74i (Handover bound for ticks where the body touches a support).
- BUG-ozxss (Bundled asset library for presets that ship scans and HDRIs).
- BUG-cb2k (scene-loop-seam-cut-under-live-param-modulation).
- BUG-f66b (adding-primitives-boundary-codegen-wording-contradicts-own-exemption).
- BUG-9sy (licensing risk decision) and BUG-qy2 (shipping-license-audit): the shipping licence question, two beads.
- BUG-fj64 (Hard rule: Peter stop-calls are session-binding).
- BUG-l0i3 (Retire Grafana usage stack and Codex tooling?) and BUG-45xz (Should Cargo cleanup delete stale hashed test/tool executables?).
- BUG-8k17w (objc2 encoding checks allocate every frame in dev builds).
- BUG-eh4 (Verdict-pass meta-decision session) and BUG-m3f6 (eiur REVIEW-twin judgment calls).
- Detector set: BUG-3k8be (Offsets: prove project songs from the snare fit instead of the kick fit?), BUG-i67g0 (Offset proof: should a fit carried by a minority of the set's notes count?), BUG-69iex (Label review: fetch online-only inputs), BUG-6jtq2 (Snare: hardneg and coincident near-misses), BUG-r19c2 (Label review: add review-proven masters to variant_proofs.json?), BUG-s05gr (Kick detector past the 84/84 ceiling), BUG-sa3n3 (Kick lane and Bad Guy bass-line fires), and child 12 of BUG-9ngk8 (Hats + percussion detector epic), the deferred frame-wise causal drum model.

The census decision bead BUG-g98wz (Peter's 18 stage and look decisions from the simplification census) is closed: all 18 have an outcome in its notes.

## Test suite cut list waiting on you

`docs/TEST_SUITE_REDUCTION_DESIGN.md` (the test suite review, child 1 of BUG-hkbdp (renderer crate split)) proposes going from 7,899 tests to 7,884 and cutting the nightly GPU leg from 2,090 s to about 1,990 s. Nothing is cut yet. Its section 10 (Decisions for Peter) asks you:

- Approve cuts C1 to C10 and C12 as written.
- C1 to C3: turn the cost measurements into examples, or delete them.
- C7: fold the finite-pixel check into the first-frame sweep and delete the smoke sweep.
- C10: move the four viewport demos to manifold-nodes-scene, or shrink them where they are.
- C11: merge the seven Liveschool fixture checks into one load (about a minute saved, the per-check names lost), or keep seven.
- Trimming the long golden audio clips is not proposed; say if you want it. The water coupling gravity tests (1,296 s, the longest single chain) go to a water bead instead.

## Live test list: things only you can check in the app on main

Rebuild main yourself, then:

- The graph editor inspector scrolls (BUG-v47j2 (Graph editor inspector does not scroll since the UI rebuild)).
- The Scene tab water lattice readout follows the Resolution card (BUG-l2itk (Scene tab water Lattice readout ignores the layer Resolution card)).
- Scene tree group headings read as headings: a darker band and subtle bold.
- The Scene tab split hairline lights on hover and drags.
- Rack Bypass and Wet/Dry, by ear.
- A new project starts on audio input None (layers only). An old show set to System Default keeps it after you re-add its only source.
- An untrimmed video beat cut lands on time, and a trimmed clip shows its in-point frame.
- Repaint checks: graph editor pan, zoom, hover and drag; picker arrows and chips; window resize; dragging the window between a 2x and a 1x display.
- A light's Type change rebuilds its rows while it stays selected (BUG-o94n7 (Verify Scene tab rebuilds when a selected light changes type)).
- The fields lane 8 moved onto one typing path commit in the live app: automation point value and time, graph group rename, graph string, WGSL, numeric, node search, table cell, save preset name.

## Notes

- The GitHub repo is public, so the `.manifold` show fixtures stay gitignored and untracked. Do not add one.
- `.claude/hooks/cargo-check-on-stop.sh` is still on disk (BUG-qfi9a (Leftovers from lane 10 process-layer deletions)). Lanes may not delete hooks, so it waits for you.
- The perf baseline identity is stale. Re-baseline before trusting a perf comparison.
- Old history snapshots open only in the legacy build.
- The heightfield_shadow palette node is gone.
- MIDI Clock position sync is still dead in the app: child 13 of BUG-mmct0 (Playback pre-Fable code audit pilot).
- Focus Distance moved under the Depth of Field switch. This goes against D4 of its design.
- The white sun disc is now (4,4,4) instead of (4,4,3.8).
- During an export every click is dropped except Cancel and Quit (your ruling). MIDI or OSC played during an export may fire when it ends (BUG-vj5w7 (MIDI and OSC input received during an export may apply when it ends)); suspected, not proven.
- Three test-render folders landed in the main checkout's `target/ui-snapshots/gltfscene`: `run-closed`, `run-preset_open`, `run-add_open`. Safe to delete.
