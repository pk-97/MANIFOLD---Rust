# AbletonOSC Patch — `arrangement_clips` extras

## What this is

A small additive patch to the user-installed [AbletonOSC](https://github.com/ideoforms/AbletonOSC) Live remote script. Adds two new OSC endpoints that AbletonOSC's stock release does not expose:

- `/live/track/get/arrangement_clips/end_time` — per-clip absolute end position in beats
- `/live/track/get/arrangement_clips/muted` — per-clip mute state

## Why MANIFOLD needs them

The perform-mode HUD's PLAY-group display shows which tracks are *supposed to be playing* at the current playhead position, derived from the arrangement timeline.

Stock AbletonOSC exposes `arrangement_clips/length` and `arrangement_clips/start_time`, but **not** `end_time`. This works fine for non-looped clips where `end = start + length`, but **breaks for looped MIDI clips** (the common case where you draw a 1-bar pattern and drag the right edge to fill 16 bars in arrangement view). Live's API returns `clip.length` as the *loop length* (1 bar), not the visible arrangement footprint (16 bars). The HUD ends up showing the track as "playing" only during the first bar of the looped region.

`clip.end_time` returns the actual visible end of the clip in arrangement view, regardless of looping. With this patch, the HUD becomes accurate.

`arrangement_clips/muted` is added at the same time so we can honor clip-level mute (Ableton lets you mute individual clips by clicking the small dot on the clip header — useful for "skip this section" workflows during prep).

## Why isn't this upstream?

Stock AbletonOSC exposes three properties for arrangement clips (`name`, `length`, `start_time`) — the maintainer added the obvious ones and didn't think about the looping-clip case. `clip.end_time` and `clip.muted` are stable, documented properties of Live's `Clip` class, both already exposed by AbletonOSC for *session* clips. The arrangement-batch query is just incomplete.

The right long-term fix is a PR upstream. Until that ships, this patch keeps MANIFOLD's perform-mode HUD reliable.

## What the patch changes

Two new function definitions and two new OSC handler registrations are added to `abletonosc/track.py`, right after the existing `track_get_arrangement_clip_*` block. Nothing existing is modified or removed.

```python
def track_get_arrangement_clip_end_times(track, _):
    return tuple(clip.end_time for clip in track.arrangement_clips)

def track_get_arrangement_clip_muted(track, _):
    return tuple(clip.muted for clip in track.arrangement_clips)
```

```python
self.osc_server.add_handler("/live/track/get/arrangement_clips/end_time",
    create_track_callback(track_get_arrangement_clip_end_times))
self.osc_server.add_handler("/live/track/get/arrangement_clips/muted",
    create_track_callback(track_get_arrangement_clip_muted))
```

## Safety

- **Additive only.** No existing endpoint is modified or removed.
- **`clip.end_time` and `clip.muted` are stable Live API properties** — present since Live 9, documented, used by other Live remote scripts.
- **Failure mode is silent**: if the patch fails to apply or is reverted, the new endpoints simply don't exist. AbletonOSC and Ableton Live continue to work normally; only MANIFOLD's perform-mode HUD degrades (PLAY-group track list will show stale/incorrect playing state for looped clips).
- **Patch is idempotent.** Running the install script twice is safe — the second run is a no-op.
- **Backup is automatic.** The install script copies the original `track.py` to `track.py.bak` before patching. Uninstall restores from the backup.

## Installing

```sh
./scripts/install-abletonosc-patch.sh
```

After running, **restart Ableton Live** so it reloads the remote script.

## Uninstalling

```sh
./scripts/uninstall-abletonosc-patch.sh
```

After running, restart Ableton Live.

## Where it's installed

The script looks for AbletonOSC at:

```
~/Library/CloudStorage/Dropbox/Music Production/Ableton/User Library/Remote Scripts/AbletonOSC/abletonosc/track.py
```

If your install lives somewhere else (e.g. you don't sync your Ableton User Library through Dropbox), set `ABLETONOSC_PATH` before running the script:

```sh
ABLETONOSC_PATH="/path/to/your/AbletonOSC/abletonosc/track.py" ./scripts/install-abletonosc-patch.sh
```

## When this patch can be removed

When [upstream AbletonOSC](https://github.com/ideoforms/AbletonOSC) adds `arrangement_clips/end_time` and `arrangement_clips/muted` to its stock distribution. Until then, anyone running MANIFOLD's perform-mode HUD against a real arrangement needs this patch.
