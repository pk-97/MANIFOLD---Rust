---
name: profiler-guide
description: How to find, read, and analyze MANIFOLD profiling sessions. Invoke when asked for a profiler rundown or when chasing frame cost.
---
# Profiler Analysis Guide

## Finding the Latest Session

```bash
ls -lt "/Users/peterkiemann/MANIFOLD - Rust/profiling_sessions/" | head -5
```

Sessions are directories named `YYYY-MM-DD_HHMMSS_<ProjectName>/` containing:
- `session.json` — metadata (resolution, FPS target, GPU, duration, frame count)
- `summary.json` — aggregated stats (LARGE — use offset/limit or Python)
- `frames.jsonl` — per-frame data (VERY LARGE — always use Python, never Read)
- `timeline.json` — project structure snapshot (layers, clips, effects at session start)

## What the Numbers Mean

**GPU pass timings are EXCLUSIVE per-pass durations.** The GpuProfiler uses wgpu `TimestampWrites` (beginning_of_pass + end_of_pass) and computes `(end - begin) * timestamp_period`. These are real GPU costs per pass, NOT overlapping wall-clock.

**However:** The sum of all pass durations will exceed the actual frame time because the GPU pipelines work internally. The actual frame time (`wall_time_ms` / `mean_frame_ms`) is what matters for "are we hitting budget?" The per-pass times tell you WHERE the time goes.

**`gpu_poll` in phase_aggregates** = `device.poll(wait_indefinitely)` — this is the CPU waiting for GPU completion. Close to `render_content` minus command recording overhead.

## Standard Analysis Script

When user asks for a profiler rundown, use this Python script via Bash (adjust session path):

```python
import json
from collections import defaultdict

SESSION = 'profiling_sessions/LATEST_DIR'

with open(f'{SESSION}/session.json') as f: meta = json.load(f)
with open(f'{SESSION}/summary.json') as f: summ = json.load(f)

# === OVERVIEW ===
budget = meta['frame_budget_ms']
res = meta['resolution']
print(f"Project: {meta['project_name']}  Resolution: {res[0]}x{res[1]}  Target: {meta['target_fps']}fps  Budget: {budget:.1f}ms")
print(f"Duration: {meta['duration_seconds']:.1f}s  Frames: {meta['total_frames']}")
print(f"Mean: {summ['mean_frame_ms']:.1f}ms  P95: {summ['p95_frame_ms']:.1f}ms  P99: {summ['p99_frame_ms']:.1f}ms  Max: {summ['max_frame_ms']:.1f}ms")
print(f"Over budget: {summ['frames_over_budget']}/{meta['total_frames']} ({100*summ['frames_over_budget']/meta['total_frames']:.0f}%)")
print(f"GPU budget usage: {summ['pass_count']['gpu_budget_usage_pct']:.0f}%  Mean passes/frame: {summ['pass_count']['mean_pass_count']:.0f}")

# === IDLE VS ACTIVE ===
iva = summ.get('idle_vs_active')
if iva:
    print(f"\nIdle: {iva['idle_mean_ms']:.1f}ms ({iva['idle_frame_count']} frames)  Active: {iva['active_mean_ms']:.1f}ms ({iva['active_frame_count']} frames)  Overhead: {iva['overhead_ms']:.1f}ms")

# === JITTER ===
j = summ['jitter']
print(f"Jitter CV: {j['coefficient_of_variation']:.2f}  Significant jitter frames: {j['frames_with_significant_jitter']}")

# === TOP GPU PASSES ===
print(f"\n{'Pass':<55s} {'Mean':>7s} {'P95':>7s} {'Count':>6s}")
print('-' * 80)
for p in summ['gpu_pass_aggregates'][:20]:
    print(f"{p['name']:<55s} {p['mean_ms']:6.1f}ms {p['p95_ms']:6.1f}ms {p['frame_count']:6d}")

# === HOTSPOTS ===
if summ['hotspots']:
    print(f"\nHotspots:")
    for h in summ['hotspots']:
        print(f"  Bar {h['bar_range'][0]}-{h['bar_range'][1]}: {h['frames_over_budget']}/{h['total_frames']} over budget, mean {h['mean_frame_ms']:.1f}ms")

# === RECOMMENDATIONS ===
print(f"\nRecommendations:")
for r in summ['recommendations']:
    print(f"  - {r}")
```

## Deep Dive: Per-Frame Analysis

When user wants to understand specific frames (heaviest, specific bars, etc.), use frames.jsonl:

```python
import json
from collections import defaultdict

SESSION = 'profiling_sessions/LATEST_DIR'
frames = []
with open(f'{SESSION}/frames.jsonl') as f:
    for line in f:
        frames.append(json.loads(line))

# Pick frames by criteria
heavy = sorted(frames, key=lambda f: f['wall_time_ms'], reverse=True)[:5]

for fr in heavy:
    clip_passes = defaultdict(list)
    layer_fx, master_fx, other = [], [], []
    for p in fr['gpu_passes']:
        name = p['name']
        if name.startswith('clip:'):
            clip_id = name.split(':')[1]
            clip_pass = name.split(':', 2)[2]
            clip_passes[clip_id].append((clip_pass, p['ms']))
        elif name.startswith('layer:'):
            layer_fx.append((name, p['ms']))
        elif name.startswith('master:'):
            master_fx.append((name, p['ms']))
        else:
            other.append((name, p['ms']))

    gen_total = sum(ms for cid in clip_passes for _, ms in clip_passes[cid])
    lfx_total = sum(ms for _, ms in layer_fx)
    mfx_total = sum(ms for _, ms in master_fx)
    oth_total = sum(ms for _, ms in other)

    print(f"Frame {fr['index']} | bar {fr['bar']} | wall {fr['wall_time_ms']:.1f}ms")
    print(f"  Generators: {gen_total:.1f}ms ({len(clip_passes)} clips)")
    print(f"  Layer FX:   {lfx_total:.1f}ms ({len(layer_fx)} passes)")
    print(f"  Master FX:  {mfx_total:.1f}ms ({len(master_fx)} passes)")
    print(f"  Other:      {oth_total:.1f}ms ({len(other)} passes)")
```

## Deep Dive: Generator Cost by Type

Group all clip passes by generator/pass type to see aggregate cost:

```python
groups = defaultdict(lambda: {'total_ms': 0, 'instances': 0})
for p in summ['gpu_pass_aggregates']:
    if not p['name'].startswith('clip:'): continue
    pass_type = p['name'].split(':', 2)[2]
    groups[pass_type]['total_ms'] += p['mean_ms']
    groups[pass_type]['instances'] += 1

for name, g in sorted(groups.items(), key=lambda x: -x[1]['total_ms']):
    avg = g['total_ms'] / g['instances']
    print(f"{name:<40s} {g['instances']:4d} inst  {avg:.1f}ms avg  {g['total_ms']:.1f}ms total")
```

## Deep Dive: Per-Bar Timeline

Show what's happening at each musical position:

```python
bar_data = defaultdict(lambda: {'n': 0, 'wall': 0, 'gen': 0, 'lfx': 0, 'mfx': 0, 'clips': set()})
for fr in frames:
    bar = fr['bar']
    bd = bar_data[bar]
    bd['n'] += 1
    bd['wall'] += fr['wall_time_ms']
    for p in fr['gpu_passes']:
        name = p['name']
        if name.startswith('clip:'):
            bd['gen'] += p['ms']
            bd['clips'].add(name.split(':')[1])
        elif name.startswith('layer:'): bd['lfx'] += p['ms']
        elif name.startswith('master:'): bd['mfx'] += p['ms']

for bar in sorted(bar_data):
    bd = bar_data[bar]
    n = bd['n']
    print(f"Bar {bar:3d}: {bd['wall']/n:5.1f}ms wall | gen {bd['gen']/n:5.1f} | lfx {bd['lfx']/n:5.1f} | mfx {bd['mfx']/n:5.1f} | {len(bd['clips'])} clips")
```

## Key Fields in frames.jsonl

Each frame has:
- `index`, `beat`, `bar`, `wall_time_ms`, `budget_exceeded`
- `content_thread`: `{total_ms, midi_input_ms, sync_controllers_ms, engine_tick_ms, render_content_ms, gpu_poll_ms, cleanup_ms}`
- `gpu_passes[]`: `{name, ms, begin_ns, end_ns, width, height, is_compute}`
- `active_clips[]`: `{clip_id, generator_type, layer_index, anim_progress, gen_params[]}`
- `active_effects[]`: `{effect_type, scope, group_id, params[]}`
- `layer_states[]`: opacity/mute/solo per layer
- `gpu_pass_count`, `gpu_total_ms`, `missed_frames`, `profiler_overhead_ms`
- `memory`: `{estimated_vram_bytes}`

## Key Fields in summary.json

- `frames_over_budget`, `worst_frame {index, ms, beat, bar}`
- `mean_frame_ms`, `p95_frame_ms`, `p99_frame_ms`, `max_frame_ms`
- `phase_aggregates`: per-CPU-phase `{mean_ms, p95_ms, p99_ms, max_ms}`
- `gpu_pass_aggregates[]`: per-label `{name, mean_ms, p95_ms, p99_ms, max_ms, frame_count, first_seen_frame, steady_state_mean_ms}`
- `hotspots[]`: `{beat_range, bar_range, mean_frame_ms, frames_over_budget, total_frames}`
- `jitter`: `{mean_dt_ms, stddev_dt_ms, coefficient_of_variation, frames_with_significant_jitter}`
- `first_use_spikes[]`: shader compilation detected (first frame >5x steady state)
- `idle_vs_active`: `{idle_mean_ms, active_mean_ms, overhead_ms, idle_frame_count, active_frame_count}`
- `pass_count`: `{mean_pass_count, max_pass_count, mean_gpu_total_ms, gpu_budget_usage_pct}`
- `recommendations[]`: auto-generated optimization suggestions

## Session Comparison

There is a `compare_sessions(dir_a, dir_b)` in `manifold-profiler/src/compare.rs` that diffs two sessions. Not exposed as a CLI tool yet — would need a small binary or script.

## Presentation Notes

- User often wants tables — use markdown tables
- Present overview first, then drill into what's interesting
- Don't stack all instances of a generator — group by what's active per-frame
- The user understands GPU rendering well — be direct, don't over-explain
- Always note resolution and FPS target — cost scales with pixel count
