# Detector Label Review — Peter checks the kick and snare labels by ear and eye in a MANIFOLD project

**Status:** IN PROGRESS · builder and read-back built on feat/label-review-builder · owed: Peter's first review pass, the overlay's merge into the label builders (section 6)
**Prerequisites:** project_tool label-review verbs (project new, layer add, clip add-bulk, clip list, marker add-bulk, marker list)

Tool: `tools/audio_analysis/lab/review_project.py` (`scripts/dev.py lab-review`). Tests:
`tools/audio_analysis/lab/test_review_project.py`.

## 1. What it is

Every project song of the lab (`lab/songs.json`) sits end to end on one MANIFOLD timeline. Peter plays it, deletes
labels that are not hits, moves labels onto the hit, adds missing ones, and drops a `reviewed` marker in each song he
finishes. The tool diffs his copy against the original and writes one overlay per family. It never edits a label
file; folding the overlay into the labels is a separate, later step.

## 2. Layout

- **One window per song, end to end.** Each window starts on a 4-beat boundary and carries the song's own tempo
  points. The bar the lab mix starts in (Ableton bar A0) lands on the window start, so every bar line inside the
  window is the set's bar line. A window ends at the end of the mix rounded up to the next bar, plus 2 empty bars.
- **Time rule.** Lab mix second t sits at arrangement second t − offset_s (the registry's proven export offset; label
  times are read as the files hold them). Its project second is the window's mix anchor + t, and its beat follows the
  project's tempo map. Audio clips are unwarped, so the mix plays in real seconds and every clip lines up with it.
- **Tempo.** Ableton tempo automation is linear in BPM between points. A step (two points on one beat) maps exactly.
  A ramp becomes steps on every quarter beat, each at the BPM that spends the ramp's own time on it, so seconds are
  exact at every quarter beat and bar line; the largest gap inside a step is reported (under 0.1 ms for a 20 BPM
  ramp over 4 bars). None of the 62 sets in use automates tempo inside its window today; the set with a tempo change
  ("All In For You", Growth Project) is not a lab song.
- **Songs without a set** (6 stem-sum songs, 5 third-party sections): a constant grid fitted to the song's kick
  labels (for a stem-sum song those are its kick stem's own attacks). Kept when 75% of kicks sit within 30 ms of a
  beat, else drawn at 120 BPM and marked `(no grid)`. The downbeat is a guess. manifold_audio's autocorrelation BPM
  put under a fifth of the kicks on its grid for most of these songs; Beat This is not installed in the lab env.
- **Time signature.** Bars are 4/4. A set whose main-track time signature is anything else is reported.

## 3. Lanes

In this order; trigger lanes are children of Drums (project_tool requires an owner layer).

| Lane | Clips |
|---|---|
| Mix | the lab mix (registry.mix), 48 kHz float WAV under `~/.cache/manifold/detector-lab/review/` |
| Drums | the catalogue's drum and kick stems summed and shifted by the unity check's proven lag; only songs whose unity check is `aligned` |
| Kick labels, Snare labels | positives and dense-run positives, 0.0625 beats each, cut at the next label |
| Kick fires, Snare fires | the shipped detectors on the same WAV (`detector_fires` example): onset to emission, at least 0.0625 beats, cut at the next fire |
| Kick unsure, Snare unsure | the labels' unscored spans, overlaps merged |

Markers: `<song> (bar N)` at each window start (`(fitted grid)` or `(no grid)` without a set), the set's locators,
and `check <family>: fires, no labels (N bars)` / `check <family>: labels, no fires (N bars)` at the first bar of
every stretch of 8 or more bars where one lane has hits and the other none (unsure bars never count as missing
labels; one quiet bar does not split a stretch).

## 4. Edit semantics

- A deleted label clip is not a hit (Peter's ruling).
- A moved label clip is a hit at its new time.
- A new clip on a label lane is a new hit. A label moved to the other family's lane is not a hit for the old family
  and a new hit for the new one.
- An unsure span that now holds a moved or new label is reported with the hits inside it.
- A `reviewed` marker inside a song's window marks the song reviewed.
- Edits to fires, unsure, mix or drums clips are counted and otherwise ignored.

## 5. Read-back contract

`build` writes `OUT.review.json` beside the project: every clip id (printed by `clip add-bulk` in input order) with
song, family, kind (`label`/`fire`/`unsure`/`mix`/`drums`) and lab time; every window's beats, seconds and mix anchor
(the mix clip's own project second as project_tool reports it). `read ORIGINAL EDITED` diffs by clip id and writes
`EDITED.review/{kick,snare}_overlay.json` (provenance `peter`, per-song `not_hits`, `moved`, `new_hits`,
`unsure_resolved`, counts, reviewed and unreviewed songs) and `summary.txt`. A moved or new hit carries `grid_s` (where
Peter put it) and `attack_s`: the steepest rise of the lab onset envelope (detector_labels.env_db in the family's
band) within ±20 ms.

## 6. Open items

- Folding an overlay into the label builders (a Peter-provenance layer the audit understands) is not designed.
- The no-grid songs (worship, gerrit, two third-party sections) review without bar lines.
- Drum stems skipped: heavy_on_mind, miracle, late_night, basalt, weight (unity check not aligned), staged (stems
  online only); 46 songs have no drum stems in the catalogue.
