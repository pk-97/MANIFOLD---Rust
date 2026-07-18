"""ODF fire attribution — DEV ONLY (2026-07-18).

Parses a MANIFOLD_ODF_DEBUG dump from `causal_events` and attributes every
miss (edm_kit) or every fire (sustained_pad) to the gate that stopped/allowed
it.  Companion to the instrumentation in
`crates/manifold-audio/src/analysis.rs`.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Dict, List, NamedTuple, Optional, Tuple

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from eval.paths import DATA_ROOT  # noqa: E402

CLASSES = ("kick", "snare", "hat", "perc")
SELF_RENDER_PITCH_TO_CLASS = {36: "kick", 38: "snare", 39: "snare", 42: "hat", 45: "perc"}
TOLERANCE_SEC = 0.05

ODF_RE = re.compile(
    r"ODFDBG\s+"
    r"band=(?P<band>\d+)\s+"
    r"candidate=(?P<candidate>[-+]?\d*\.?\d+)\s+"
    r"median=(?P<median>[-+]?\d*\.?\d+)\s+"
    r"threshold=(?P<threshold>[-+]?\d*\.?\d+)\s+"
    r"novelty_ref=(?P<novelty_ref>[-+]?\d*\.?\d+)\s+"
    r"novel=(?P<novel>true|false)\s+"
    r"is_peak=(?P<is_peak>true|false)\s+"
    r"refr=(?P<refr>\d+)\s+"
    r"fired=(?P<fired>true|false)"
)


class OdfLine(NamedTuple):
    band: int
    time_sec: float
    candidate: float
    median: float
    threshold: float
    novelty_ref: float
    novel: bool
    is_peak: bool
    refr: int
    fired: bool


def parse_dump(path: Path) -> Tuple[List[OdfLine], float, float]:
    text = path.read_text()
    hop_match = re.search(r"HOPDBG\s+hop_samples=(\d+)\s+sample_rate=(\d+)", text)
    if not hop_match:
        raise SystemExit(f"no HOPDBG line in {path}")
    hop_samples = int(hop_match.group(1))
    sample_rate = int(hop_match.group(2))
    hop_sec = hop_samples / sample_rate

    raw = [m.groupdict() for m in ODF_RE.finditer(text)]
    if not raw:
        raise SystemExit(f"no ODFDBG lines in {path}")

    # Bands are emitted in order per hop; infer hop index from the cycle.
    bands_per_hop = max(int(r["band"]) for r in raw) + 1
    lines: List[OdfLine] = []
    for i, r in enumerate(raw):
        hop_index = i // bands_per_hop
        lines.append(
            OdfLine(
                band=int(r["band"]),
                time_sec=hop_index * hop_sec,
                candidate=float(r["candidate"]),
                median=float(r["median"]),
                threshold=float(r["threshold"]),
                novelty_ref=float(r["novelty_ref"]),
                novel=r["novel"] == "true",
                is_peak=r["is_peak"] == "true",
                refr=int(r["refr"]),
                fired=r["fired"] == "true",
            )
        )
    return lines, hop_sec, sample_rate


def load_self_render_truth(name: str) -> List[float]:
    truth_path = DATA_ROOT / "self_render" / f"{name}_truth.json"
    if not truth_path.exists():
        raise SystemExit(f"truth missing: {truth_path}")
    times: List[float] = []
    for n in json.loads(truth_path.read_text()):
        cls = SELF_RENDER_PITCH_TO_CLASS.get(n["pitch"])
        if cls:
            times.append(float(n["start_sec"]))
    return sorted(times)


def nearest(items: List[OdfLine], t: float) -> Optional[OdfLine]:
    if not items:
        return None
    return min(items, key=lambda x: abs(x.time_sec - t))


def classify_miss(peak: OdfLine) -> Tuple[str, Dict[str, float]]:
    values = {
        "candidate": peak.candidate,
        "median": peak.median,
        "threshold": peak.threshold,
        "novelty_ref": peak.novelty_ref,
        "candidate_over_threshold": (
            peak.candidate / peak.threshold if peak.threshold > 0 else float("inf")
        ),
        "candidate_over_novelty_floor": (
            peak.candidate / (peak.novelty_ref * 2.0 + 125.0)
            if (peak.novelty_ref * 2.0 + 125.0) > 0
            else float("inf")
        ),
    }
    if peak.refr > 0:
        return "refractory", values
    if peak.candidate <= peak.threshold and not peak.novel:
        return "below_both", values
    # Defensive: any other failed peak shape.
    return "other", values


def classify_admit(line: OdfLine) -> Tuple[str, Dict[str, float]]:
    values = {
        "candidate": line.candidate,
        "median": line.median,
        "threshold": line.threshold,
        "novelty_ref": line.novelty_ref,
    }
    median_hit = line.candidate > line.threshold
    if median_hit and line.novel:
        return "both", values
    if median_hit:
        return "median", values
    if line.novel:
        return "novelty", values
    return "other", values


def analyze_edm_kit(dump: Path) -> None:
    lines, hop_sec, _sr = parse_dump(dump)
    truth = load_self_render_truth("edm_kit_128bpm")

    fired_lines = [ln for ln in lines if ln.fired]
    peak_lines = [ln for ln in lines if ln.is_peak]

    misses: List[Tuple[float, OdfLine, str, Dict[str, float], float]] = []
    for t in truth:
        if any(abs(ln.time_sec - t) <= TOLERANCE_SEC for ln in fired_lines):
            continue
        peak = nearest(peak_lines, t)
        if peak is None or abs(peak.time_sec - t) > TOLERANCE_SEC:
            misses.append((t, None, "no_peak", {}, float("inf")))
            continue
        reason, values = classify_miss(peak)
        misses.append((t, peak, reason, values, abs(peak.time_sec - t)))

    counts: Dict[int, Dict[str, int]] = {}
    for _t, peak, reason, _values, _dt in misses:
        band = peak.band if peak is not None else -1
        counts.setdefault(band, {}).setdefault(reason, 0)
        counts[band][reason] += 1

    print(f"edm_kit_128bpm: {len(truth)} truth events, {len(misses)} misses ({len(misses)/max(len(truth),1)*100:.1f}%)")
    print(f"hop size: {hop_sec*1000:.2f} ms")
    print("\nmiss-reason counts per band:")
    print(f"{'band':>6}  {'below_both':>12}  {'refractory':>12}  {'no_peak':>10}  {'other':>8}")
    for band in sorted(counts):
        c = counts[band]
        print(
            f"{band:>6}  {c.get('below_both', 0):>12}  {c.get('refractory', 0):>12}  "
            f"{c.get('no_peak', 0):>10}  {c.get('other', 0):>8}"
        )

    print("\n5 example misses:")
    print(
        f"{'truth_s':>10}  {'band':>6}  {'reason':>12}  {'dt_ms':>8}  "
        f"{'candidate':>10}  {'threshold':>10}  {'novel':>7}  {'refr':>6}"
    )
    for t, peak, reason, values, dt in misses[:5]:
        if peak is None:
            print(f"{t:>10.3f}  {'-':>6}  {reason:>12}  {'-':>8}")
            continue
        print(
            f"{t:>10.3f}  {peak.band:>6}  {reason:>12}  {dt*1000:>8.2f}  "
            f"{peak.candidate:>10.1f}  {peak.threshold:>10.1f}  "
            f"{str(peak.novel).lower():>7}  {peak.refr:>6}"
        )
        if reason == "below_both":
            print(
                f"            margins: candidate/threshold={values.get('candidate_over_threshold', 0):.3f}, "
                f"candidate/(novelty_ref*2+125)={values.get('candidate_over_novelty_floor', 0):.3f}"
            )


def analyze_pad(dump: Path) -> None:
    lines, hop_sec, _sr = parse_dump(dump)

    fired = [ln for ln in lines if ln.fired]
    counts: Dict[int, Dict[str, int]] = {}
    examples: List[Tuple[OdfLine, str, Dict[str, float]]] = []
    for ln in fired:
        reason, values = classify_admit(ln)
        counts.setdefault(ln.band, {}).setdefault(reason, 0)
        counts[ln.band][reason] += 1
        examples.append((ln, reason, values))

    print(f"\nsustained_pad_100bpm: {len(fired)} ODF fires")
    print(f"hop size: {hop_sec*1000:.2f} ms")
    print("\nadmit-criterion counts per band:")
    print(f"{'band':>6}  {'median':>10}  {'novelty':>10}  {'both':>8}  {'other':>8}")
    for band in sorted(counts):
        c = counts[band]
        print(
            f"{band:>6}  {c.get('median', 0):>10}  {c.get('novelty', 0):>10}  "
            f"{c.get('both', 0):>8}  {c.get('other', 0):>8}"
        )

    print("\n5 example fires:")
    print(
        f"{'time_s':>10}  {'band':>6}  {'criterion':>10}  {'candidate':>10}  "
        f"{'median':>10}  {'threshold':>10}  {'novelty_ref':>12}  {'novel':>7}"
    )
    for ln, reason, _values in examples[:5]:
        print(
            f"{ln.time_sec:>10.3f}  {ln.band:>6}  {reason:>10}  {ln.candidate:>10.1f}  "
            f"{ln.median:>10.1f}  {ln.threshold:>10.1f}  {ln.novelty_ref:>12.1f}  "
            f"{str(ln.novel).lower():>7}"
        )


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--edm-dump", type=Path, default=Path("/tmp/odfdbg_edm.txt"))
    parser.add_argument("--pad-dump", type=Path, default=Path("/tmp/odfdbg_pad.txt"))
    args = parser.parse_args(argv)

    analyze_edm_kit(args.edm_dump)
    analyze_pad(args.pad_dump)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
