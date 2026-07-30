You are a mechanical code-change generator. Read the brief and the reference files, then output ONLY a JSON object, no prose, no markdown fences:

{"edits": [{"path": "<repo-relative path>", "find": "<exact text currently in the file>", "replace": "<replacement text>"}, ...],
 "writes": [{"path": "<repo-relative path>", "content": "<full file content>"}],
 "commit_message": "<one line>"}

# Brief: MB-C — in-repo regression pin for multi-bounce GI (RAYTRACING_DESIGN.md section 11.4 MB-B gate, final clause)

Two deliverables:

1. A new gpu-proofs test file `crates/manifold-renderer/tests/gpu_proofs/rt_t38_multibounce.rs` (in `writes`).
2. One edit registering it in `crates/manifold-renderer/tests/gpu_proofs/main.rs`: add `mod rt_t38_multibounce;` in alphabetical position among the existing `mod rt_*;` lines (copy the exact style of the neighbouring lines).

## The test file

Module doc comment: state that this pins the SHIPPED 2-bounce GI behaviour
(RAYTRACING_DESIGN.md section 11, MB4/MB5/I-MB2) as a regression floor — the causal
1-vs-2 bounce proof ran once, cross-commit, in the workflow program's gate
(scripts/rt_region_probe.py); this test keeps the result from silently regressing.

Copy the harness pattern from the reference file `rt_p3_emissive_gi.rs` below: the same
`harness::shared()`, `PresetRuntime::from_json_str_with_device`, warm-up render loop, and
readback/decoding helpers it uses (imports included). Warm up for 60 frames (not 16) —
the bleed signal is accumulation-dependent.

The scene JSON is NOT inline: load both fixtures from the repo with
`std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/rt_prototype/compare/RtBleed.json"))`
(and the same for `RtAmbientOnly.json`).

Probe rect, in fractions of the render size (identical fixtures render identically at any
square size; the reference rect was measured at 512x512): x from 80/512 to 230/512 of
width, y from 175/512 to 235/512 of height. Compute a mean over that rect of (red -
green) in LINEAR values (the readback is already linear — no tonemap step; decode texels
exactly the way the reference file's readback helpers do).

Two #[test] functions:

- `multibounce_bleed_region_reads_red_tinted`: render RtBleed, assert
  `rg_mean > PIN_THRESHOLD` with
  `const PIN_THRESHOLD: f32 = <the "pin_threshold" value from the probe JSON below>;`
  — document the constant: "set from the workflow run's measured 1-vs-2 delta / 2
  (probe-bleed artifact, run rt-t38-multibounce); floor 0.006".
- `ambient_only_region_stays_neutral`: render RtAmbientOnly, assert
  `rg_mean.abs() < 0.005` — env is never gathered at any depth (I-MB2), so with zero
  emissive and zero lights nothing tints the floor.

Both tests print the measured value on failure (assert! with a formatted message).

commit_message: "MB-C: rt_t38_multibounce gpu-proof — bleed pin + ambient-neutral leg (RAYTRACING_DESIGN.md section 11 I-MB2)"

# Probe result (the workflow's measured 1-vs-2 delta)

{{probe-bleed}}

# Reference: rt_p3_emissive_gi.rs (copy its harness/readback patterns)

{{file:crates/manifold-renderer/tests/gpu_proofs/rt_p3_emissive_gi.rs}}

# Current main.rs (for the mod registration edit)

{{file:crates/manifold-renderer/tests/gpu_proofs/main.rs}}
