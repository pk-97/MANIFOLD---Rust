# Video Playback Engine — Unity Parity Gaps

> Deferred gaps from the clip scheduling/sync audit (2026-03-17).
> These only matter once a real video decoder backend exists in Rust.
> The Unity source files are the single source of truth for all behavior described here.

## Source Files

| Component | Unity Path |
|---|---|
| PlaybackEngine | `Assets/Scripts/Playback/PlaybackEngine.cs` |
| VideoTimeCalculator | `Assets/Scripts/Playback/VideoTimeCalculator.cs` |
| IClipRenderer | `Assets/Scripts/Playback/IClipRenderer.cs` |
| PlaybackController | `Assets/Scripts/Playback/PlaybackController.cs` |
| VideoPlayerPool | `Assets/Scripts/Playback/VideoPlayerPool.cs` |
| EditMode Tests | `Assets/Tests/EditMode/VideoTimeCalculatorTests.cs` |

---

## 1. VideoTimeCalculator Bugs

### 1A. No negative local time clamp
**Unity** `VideoTimeCalculator.cs:32`: `Mathf.Max(0f, currentTime - clipStartTime)`
**Rust** `video_time.rs:11`: Raw subtraction — produces negative video time when `currentTime < clipStartTime`.

### 1B. No negative playback rate clamp
**Unity** line 33: `Mathf.Max(0f, playbackRate)` prevents reverse playback glitches.
**Rust**: Uses raw rate. Negative rate produces negative source time.

### 1C. Full-video loop uses wrong loop length
**Unity** lines 40-41: Full loop = `mediaLength - inPoint` (only loops through usable portion after InPoint).
**Rust** lines 16-19: Full loop = `mediaLength` (loops through entire video including before InPoint).

**Test proof**: Unity test `Loop_FullLength_WithInPoint` expects `mediaLength=10, inPoint=3, localTime=12 → 12 % 7 = 5 → result=8`. Rust computes `12 % 10 = 2 → result=5` (wrong).

### 1D. Loop guard threshold too small
**Unity**: `> 0.01f` for both media length and loop length. Prevents near-zero modulo.
**Rust**: `> 0.0` — could modulo by values like `0.0001` producing jitter.

### 1E. Rate scaling of custom loop duration missing
**Unity** line 40: `loopDurationSeconds * safeRate`, then clamped against `mediaLength - inPoint`.
**Rust**: Uses raw `loop_duration_seconds` without rate scaling.

**Test proof**: Unity test `Loop_CustomDuration_WithPlaybackRate_ScalesLoopLength`: `loopDuration=4s, rate=0.5 → effective loop=2s. sourceLocal=3 → 3%2=1`. Rust would compute `3%4=3` (wrong).

### 1F. Engine ComputeVideoTime uses beat-domain source elapsed
**Unity** `PlaybackEngine.cs:1561-1581`: The runtime version uses `GetClipSourceElapsedSeconds()` which computes `elapsedBeats * recordedSpb` when the clip has a `RecordedBpm`. This gives BPM-accurate source positioning.
**Rust**: Only the simpler seconds-domain `compute_video_time()` exists. The beat-domain path with `recorded_bpm` + `TempoMap` is absent.

### 1G. Beat-based overload missing
**Unity** `VideoTimeCalculator.cs:56-78`: Overload accepting `loopDurationBeats + currentSpb`.
**Rust**: Only seconds-based API. Caller must convert manually.

---

## 2. Prepare Phase (Async Decode)

### 2A. CheckPreparingClips not implemented
**Unity** `PlaybackEngine.cs:758-815`: Poll-based readiness check every frame. When a clip finishes preparing:
1. Seek to correct video time via `ComputeVideoTime()`
2. Apply playback rate
3. Set native looping if applicable
4. Resume playback
5. Add to `recentlyStartedTimes` (compositor exclusion gate)
6. Set pending pause if not playing
7. Set compositor dirty deadline

**Rust**: `preparing_clips: HashSet` field exists but is never populated or polled. All clips assumed instantly ready.

### 2B. Hot start path missing
**Unity** `PlaybackEngine.cs:647-676`: Pre-warmed players (already prepared) skip the prepare phase:
- Seek immediately to correct position
- Resume playback
- Do NOT add to `recentlyStartedTimes` (already have valid RT content)
- Set pending pause if not playing
- Replenish warm cache
- Mark compositor dirty

**Rust**: No pre-warm detection. All clips go through the same path.

---

## 3. Video Seek on Start

**Unity** `StartClip` → hot or cold path → `renderer.SeekClip(clipId, ComputeVideoTime(...))` — positions the video at the correct source time based on current playhead.

**Rust** `engine.rs:351`: `renderer.start_clip(clip, self.current_time)` — starts at whatever position the renderer defaults to (time 0). Videos play from the wrong position when playhead isn't at clip start.

---

## 4. SeekActiveClips

**Unity** `PlaybackEngine.cs:967-987`: After a transport seek, re-seeks all active prepared players to their correct positions. Called from `PlaybackController.Seek()` after `SyncClipsToTime()`.

**Rust** `engine.rs:228-237`: `seek_to()` calls `sync_clips_to_time()` but doesn't re-seek existing active clips. Active video players show the wrong frame after seek.

---

## 5. Playback Rate / BPM Time-Stretching

**Unity** `PlaybackEngine.cs:1494-1505`:
- `GetClipPlaybackRate()`: Computes rate from `RecordedBpm / currentTimelineBpm`, clamped to `[0.05, 8.0]`
- `ApplyClipPlaybackRate()`: Pushes rate to renderer via `SetClipPlaybackRate()`
- `UpdateActiveClipPlaybackRates()`: Runs every frame during playback to track tempo changes

**Rust**: None. All clips play at 1x regardless of BPM. The `set_clip_playback_rate` method exists on the renderer trait but is never called.

---

## 6. Drift Correction

**Unity** `PlaybackEngine.cs:859-947` — `CorrectVideoDrift()` runs periodically (every `videoSyncInterval` seconds, default 2s):

1. **Out-point enforcement**: Stops clips past their source duration
2. **EOF handling for live slots**: Re-seek to InPoint on video EOF (freezes on last frame)
3. **Stopped-player recovery**: Re-start players that stopped unexpectedly
4. **Seek-based drift correction**: When `|playerTime - expectedTime| > 0.1s`, re-seek
5. **Looping clips skipped**: Managed by native looping + custom boundary check

**Rust**: None. No drift detection or correction.

---

## 7. Custom Loop Boundary Enforcement

**Unity** `PlaybackEngine.cs:820-854` — `CheckCustomLoopBoundaries()` runs every frame:
- Only for clips with `LoopDurationBeats > 0`
- Skips generators (`!renderer.NeedsPreparePhase`)
- Computes boundary: `InPoint + min(sourceLoopDurationSeconds, sourceAvailable)`
- When `playerTime >= boundary`: pause → seek to InPoint → apply rate → resume

**Rust**: None. Custom loop duration is ignored at runtime.

---

## 8. Compositor Filtering (FilterReadyClips)

**Unity** `PlaybackEngine.cs:1193-1239`:
1. Resolve should-be-active from frame cache or fresh query
2. Merge live slots into fallback list
3. Call `PreRender()` on all renderers (generators blit shaders)
4. Filter out clips not ready or recently started
5. Apply proportional recently-started gate: `min(gateTime, remaining * 0.4f)`
6. Use shorter gate for live clips (`LiveRecentlyStartedTime = 0.02s`)
7. Sort by `LayerIndex` descending (back-to-front for compositing)

**Rust** `engine.rs:310-320`: Inline filtering — no `PreRender()` call, no proportional gate, no cache hit path.

---

## 9. Recently-Started Proportional Gate

**Unity** `PlaybackEngine.cs:1090-1105` — `ShouldExcludeRecentlyStarted()`:
- Standard gate: `RecentlyStartedTime = 0.1s`
- Live clip gate: `LiveRecentlyStartedTime = 0.02s`
- Proportional reduction: `gateTime = min(gateTime, remaining * 0.4f)` — prevents the gate from exceeding 40% of the clip's remaining lifetime

**Rust**: Fixed `RECENTLY_STARTED_TIME` with no proportional reduction or live-clip distinction.

---

## 10. ResumeReadyClips on Play

**Unity** `PlaybackController.cs:648-652`: On Play, resumes paused clips that were pre-warmed during Stop/LoadProject. Iterates active renderers with `NeedsPreparePhase`, checks `IsClipReady && !IsClipPlaying && !preparing`.

**Rust**: No equivalent. Paused pre-warmed clips stay paused after Play.

---

## 11. Pre-Warm System

**Unity** `PlaybackEngine.cs:1251+` — `ComputePrewarmCandidates()`:
- Lookahead window: `-0.25s` behind to `+8s` ahead
- Respects solo/mute
- Throttled: every 0.5s (normal) or 0.1s (live burst mode within 3s of last MIDI trigger)
- Max 12 unique clips (lookahead) + 12 (live) + 20 combined cap
- Change detection: only returns non-null when prewarm set has changed

Driver executes via `VideoPlayerPool.PreWarmClips()`.

**Rust**: None.

---

## 12. ClipRenderer Trait Gaps

### 12A. `OnProjectLoaded` missing
**Unity** `IClipRenderer.OnProjectLoaded(Project)`: Called when project changes. Renderers cache project-level references (e.g., VideoLibrary for source file resolution).

### 12B. `GetTexture` missing
**Unity** `IClipRenderer.GetTexture(string clipId)`: Compositor reads rendered output per clip. Platform-specific — will need a wgpu texture handle or similar in Rust.

---

## Implementation Order (Suggested)

When building the video playback engine, fix these in this order:

1. **VideoTimeCalculator bugs (1A-1E)** — pure math, easy to fix, high impact
2. **Video seek on start (3)** — clips must start at the right frame
3. **Prepare phase (2A-2B)** — async decode is fundamental to video playback
4. **SeekActiveClips (4)** — scrubbing must show correct frame
5. **Playback rate (5)** — BPM-synced video speed
6. **Drift correction (6)** — long-running playback accuracy
7. **Custom loop boundary (7)** — per-frame enforcement for loop-duration clips
8. **Compositor filtering (8-9)** — RT warmup exclusion
9. **Pre-warm (11)** — performance optimization, can ship without
10. **ResumeReadyClips (10)** — polish for pause/play transitions

Port the Unity EditMode tests (`VideoTimeCalculatorTests.cs`, 14 tests) first — they define the correct behavior and several Rust tests will need updating.
