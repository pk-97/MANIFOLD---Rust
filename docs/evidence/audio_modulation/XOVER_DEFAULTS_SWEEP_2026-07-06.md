# Band-crossover defaults sweep — 2026-07-06 (report only; defaults unchanged pending Peter's call)

Peter's secondary ask from the BUG-046 session: data-check the default band
crossovers (Low<250 Hz, Mid<2000 Hz) against the 25 real fixtures. Run by a
Sonnet agent (20 pairs × 25 clips = 500 mod_harness runs, all clean; scoring
script preserved alongside the raw table in the session artifacts, method
summarized here). Fires counted as rising edges; drums-Low on-grid% scored
against the 8th-note grid with the per-clip offset fitted on the baseline
pair then frozen. NOTE: this sweep cannot and does not address BUG-046 —
kick and bass share bins at any crossover (measured at the bin level).

## Verdict

- **Low = 250 Hz: keep — data-confirmed, not just incumbent.**
  - Raising to 300/350: bass containment improves (0.818 → 0.845/0.864) but
    drums-Low gains off-grid extra fires (apricots 13→28, on-grid 46%→36%)
    and the bad_guy mix-Low count more than doubles (6→11/13) — verified to
    be regularly-spaced bass-note onsets entering the Low detector, i.e. Low
    stops meaning "kicks" on that mix. Fails the mix-sanity gate.
  - Lowering to 150/200: bass containment drops (0.73/0.78) and bass-stem
    Mid leakage fires rise (31.4 → 36.0/33.4).
- **Mid = 2000 Hz: 1500 narrowly wins the weighted score, but the honest
  read is marginal.** Gains: vocals Mid presence +4% (0.276→0.287), drums
  Mid fires −3.5%. Costs: vocals' Mid amplitude share −6% (0.599→0.562 —
  vocal consonant/sibilance energy in 1.5–2 kHz moves to High), bass
  containment −0.6% (noise-level). The headline score gap is z-score
  inflation on small-variance metrics; absolute deltas are small.
  **Recommendation: (250, 1500) if optimizing on this data; (250, 2000) is
  not meaningfully broken — a defensible null result.**

## Aggregate table (means over 5 tracks, top rows by score + baseline)

| low | mid | dLowF | onGrid% | dMidF | dHighF | bassCont | bMidF | vMidPres | vMidShr | score | gate |
|----:|----:|------:|--------:|------:|-------:|---------:|------:|---------:|--------:|------:|------|
| 250 | 1500 | 28.8 | 67.0 | 49.0 | 74.0 | 0.813 | 31.2 | 0.2871 | 0.562 | 5.40 | clean |
| 350 | 1500 | 34.8 | 65.2 | 49.0 | 74.0 | 0.859 | 31.0 | 0.2922 | 0.527 | 4.37 | FAIL (bad_guy mix 13 vs 6) |
| 200 | 1500 | 26.8 | 66.8 | 49.2 | 74.0 | 0.782 | 33.4 | 0.2885 | 0.566 | 3.75 | clean |
| 300 | 1500 | 34.0 | 64.6 | 48.6 | 74.0 | 0.840 | 30.4 | 0.2839 | 0.554 | 3.73 | FAIL (bad_guy 11 vs 6) |
| 250 | 2000 | 28.8 | 67.0 | 50.8 | 73.6 | 0.818 | 31.4 | 0.2762 | 0.599 | 2.54 | BASELINE, clean |
| 250 | 2500 | 28.8 | 67.0 | 55.4 | 73.6 | 0.821 | 31.4 | 0.2722 | 0.609 | 1.21 | clean |
| 150 | any | 23.8 | 64.0 | 49–58 | 73.6–74 | 0.73–0.74 | 36 | 0.27–0.29 | 0.56–0.62 | ≤−1.16 | FAIL |

Full 20-row table, per-track details, and anomalies (bad_guy whisper-vocal
vMidShr=1.000 is real — its vocals stem has literally zero Low/High energy;
also its Mid presence is 0.051 at ANY crossover, a presence-detector note,
not a crossover one) are in the session transcript/artifacts. On-grid% has a
30–41% chance floor at ±35 ms, so it's a weak (but real) discriminator.
