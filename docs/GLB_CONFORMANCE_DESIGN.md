# GLB Conformance — drop in any glb and it renders accurately

**Status: IN PROGRESS · 2026-07-15 · Fable 5 (authored) + Sonnet 5 (G-P1+G-P2 executed and landed same day, `909976d2`; G-P3+G-P4+G-P5 executed and landed same day, session 2; G-P6 executed and landed same day, session 3, `017e1e41`). G-P1 (conformance harness) + G-P2 (cap deleted, import is 1:1, BUG-163 fixed as a side effect) + G-P3 (anisotropic filtering) + G-P4 (KHR_texture_transform all five map families + specular/ior F0) + G-P5 (clearcoat lobe, factor-only) + G-P6 (node.hdri_source, env_mode card switch, Softbox stays default) SHIPPED. G-P7 (burn-down) not yet executed.**
**Prerequisites: IMPORT_FIDELITY F-P1–F-P7 (all SHIPPED 2026-07-15, `44b921cf`). Nothing else.**
**Execution contract: read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase. Executed as a Sonnet→Sonnet orchestration; every phase brief is written to be run with nobody in the room.**

Peter, 2026-07-15: **"I would like our system to be able to drop in any glb and
accurately render it"** and, on the importer's texture rationing: **"why is there a
cap at all? everything should just port in 1 for 1 and map correctly."** Those two
quotes are this design's charter.

The governing insight: "any glb" is a bounded, testable claim — glTF 2.0 core plus a
short list of KHR material extensions — and Khronos publishes a sample-asset suite
built to exercise exactly that surface, feature by feature. The failure mode this
design retires is the one that burned the 2026-07-15 session: five shipped phases of
IMPORT_FIDELITY, each individually gated and green, while the only whole-asset oracle
was "does Peter think it looks right in the app." Every fix was real; every new asset
exposed the next silent gap. The fix is an **external ground truth wired into the
repo**: a headless conformance harness that renders every sample asset through the
production import path, gates each glTF feature with a numeric assert, and pins
regressions with self-goldens. After the burn-down, a new glb draws from a verified
spec surface instead of an asset-by-asset debugging queue.

On stage this means: Peter drops a purchased or downloaded model into a set and
trusts it — no per-asset surgery the night before a gig, no "why is my car black"
at soundcheck.

Companions: `IMPORT_FIDELITY_DESIGN.md` (the shading/lighting machinery this builds
on — SHIPPED, do not reopen its decisions), `IMPORT_DESIGN.md` (owns non-material
import concerns: lights/cameras/report surface; its §8 already names Khronos fixtures
— this doc is where that intent becomes real), `MATERIAL_SYSTEM_DESIGN.md` (the
Material port type contract).

---

## 1. Audit — what exists (verified 2026-07-15, live in-session)

Every row below was verified by running code this same day, not recalled.

| Piece | Where | State |
|---|---|---|
| Production import path | `assemble_import_graph` — `crates/manifold-renderer/src/node_graph/gltf_import.rs:333` | Works; builds group-per-material graph, wires all five PBR map ports, softbox+fill env (F-P7), sun, camera, post nodes |
| Full PBR map set + split-sum IBL + mips + fill | IMPORT_FIDELITY F-P1–F-P7, all landed at `44b921cf` | SHIPPED; value-level gpu proofs green |
| **Object cap — renderer side** | `OBJECT_SLIDER_MAX = 64` at `crates/manifold-renderer/src/node_graph/primitives/render_scene.rs:97`; `objects.clamp(1, OBJECT_SLIDER_MAX)` at `render_scene.rs:546` | The renderer itself refuses >64 objects, despite its own purpose string saying "no fixed cap on object count" |
| **Object cap — importer side** | `gltf_import.rs:378` (largest-by-vertex-count-first triage), `ImportReport.dropped_over_cap` (`gltf_import.rs:63`) | Materials beyond 64 are **dropped from the graph entirely** — geometry ceases to exist. Proven consequence: the AMG GT3 (78 materials) loses 14, including body panels (BUG-163) |
| Extension parse state | loader `gltf_load.rs:574-586` parses emissive factor/texture/strength; `KHR_materials_emissive_strength` folds into `emission_intensity` (`gltf_import.rs:659-663`); clearcoat/texture_transform/specular/ior parse via the gltf crate but map to **report lines only** (D9 doctrine) | Report-line doctrine works — nothing silently dropped — but nothing renders either |
| Emissive term | whole path verified value-level 2026-07-15: map correct on the wire (max 0.502 vs expected 0.5), uniform `[1,1,1]` at draw, add-path proven by unwired-glow probe | **NOT broken.** Was visually drowned pre-F-P6 by strip specular ~10× brighter. No code change needed; G-P1 pins it with a conformance case |
| Sampler | `GpuSamplerDesc` — `crates/manifold-gpu/src/types.rs:144-154`: min/mag/mip filters, address modes, compare. **No anisotropy field.** Material maps sample via the dedicated REPEAT `material_sampler` (binding 22, landed `85b5bb9d` same day — the striped-helmet root cause was the shared clamp-V envmap sampler pinning out-of-range V, e.g. DamagedHelmet's V∈[1,2], to the texture edge row) | Wrap is FIXED; what remains for G-P3 is genuine anisotropy: glancing-angle minification still over-blurs. Metal supports `maxAnisotropy`; field is genuinely new |
| EXR decode | `image = { version = "0.25", default-features = false, features = ["png","jpeg","webp","bmp","gif"] }` — `crates/manifold-renderer/Cargo.toml:26` | `exr` feature exists in image 0.25 but is not enabled; no HDR file source primitive exists (verified: `rg -l "exr" crates/` → no runtime hits) |
| Headless import-render harness | **Scratchpad only** — the 2026-07-15 probe (`envprobe`, session scratch): renders `assemble_import_graph` output through `PresetRuntime::from_def_with_device`, convergence-polls background decodes, writes PNG | Proven diagnostic value (found/killed 6 hypotheses in one session) but **dies with the session**. Port target precedent: `crates/manifold-renderer/src/bin/render_generator_preset.rs` |
| Display transform | The probe used Reinhard-without-sRGB-encode and rendered systematically darker than the app; `crates/manifold-renderer/src/bin/generate_preset_thumbnails.rs` has the in-repo readback/encode precedent | ⚠ the harness must NOT invent its own transform (D2) |
| Khronos sample assets | Not in repo. `tests/fixtures/gltf/README.md` (tier 1) and `IMPORT_DESIGN.md` §8 already call for them; never fetched | Fetch script is genuinely new |
| Committed fixtures | `tests/fixtures/gltf/DamagedHelmet.glb` (CC-BY, committed) + blossom/azalea photoscans | Usable as-is |
| Untracked local fixtures | `mercedes-amg_gt3__www.vecarz.com.glb` (licensing unverified — **never commit**), `kloppenheim_07_puresky_4k.exr` (Poly Haven CC0, 4096×2048 equirect — keep untracked for repo size; fetchable by script) | Held-out + HDRI demo material |
| Import-graph gpu tests | `gltf_import.rs` tests: helmet renders non-degenerate + wires all five maps; AMG smoke (`if present` skip pattern) | The skip-if-absent pattern for uncommitted fixtures is established here |

Extend, don't redesign: the harness is a port of a proven probe; the fetch is a
script plus the established skip-if-absent test pattern; the cap fix is a deletion;
extensions ride the existing report-line + Material seams. Genuinely new: the
anisotropy field, the clearcoat shader lobe, `node.hdri_source`, and the conformance
manifest format.

## 2. Decisions

- **D1 — Ground truth = the Khronos glTF-Sample-Assets suite, fetched by script,
  never vendored.** `scripts/fetch-gltf-conformance.sh` downloads a pinned commit of
  the suite's glb variants into `tests/fixtures/gltf/khronos/` (gitignored).
  Conformance tests skip-if-absent with a loud `SKIPPED (run
  scripts/fetch-gltf-conformance.sh)` line — the established AMG pattern. Rejected:
  vendoring the assets (hundreds of MB, mixed licenses); requiring network in tests
  (CI and worktrees must stay offline-green).
- **D2 — The harness is the production path plus the app's own output transform,
  shared by construction.** One binary, `render_import`
  (`crates/manifold-renderer/src/bin/render_import.rs`), shaped like
  `render_generator_preset.rs`: `assemble_import_graph` → `PresetRuntime` →
  converged readback → PNG. The tonemap/encode used for readback is extracted into
  ONE shared function (new module `crates/manifold-renderer/src/headless_readback.rs`)
  that `render_import`, `generate_preset_thumbnails`, and the conformance tests all
  call. **Rejected: a harness-local tonemap** — the 2026-07-15 probe had one and its
  renders diverged from the app all session (DESIGN_AUTHORING §4's
  reimplement-and-verify carve-out: share the seam, don't audit the match).
- **D3 — Gates are per-feature numeric asserts plus self-goldens, never
  pixel-matching Khronos's reference renders.** Khronos's goldens come from a
  different renderer; matching them byte-wise is a fool's errand. Instead each
  feature asset gets an assert of the form "feature on vs feature off changes the
  named quantity in the predicted direction" (the F-P2 gpu-test style, e.g.
  EmissiveStrength: lights-off render of the emissive asset has non-black fraction
  above a stated floor). Regression pinning: our OWN renders become goldens
  (`tests/fixtures/gltf/goldens/`, small PNGs, committed) with a mean-abs-diff
  tolerance of 2/255 — regenerated only by an explicit
  `UPDATE_CONFORMANCE_GOLDENS=1` run whose diff Peter reviews.
- **D4 — The cap dies. Import is 1:1; the card curates exposure, not existence.**
  Peter's quote above is the decision. `render_scene`'s clamp rises to a safety
  bound of 1024 (a real GPU limit guard, not a UI convenience — hit it and the
  import *errors loudly*, never truncates); the importer wires EVERY material's
  object; the card exposes per-object sliders for the largest 16 objects only
  (pure UI curation — `card_params` stops growing, the graph doesn't).
  `dropped_over_cap` is deleted from `ImportReport` (compiler-driven: removing the
  field surfaces every consumer). Rejected: raising 64 to a bigger magic number
  (same bug, later); merging same-material meshes at import (destroys per-object
  transforms/sliders users already rely on).
- **D5 — v1 extension coverage, exactly this list:** `KHR_materials_emissive_strength`
  (already mapped — conformance case only), `KHR_texture_transform` (UV affine —
  without it any atlas-remapped asset samples garbage; the AMG uses it),
  `KHR_materials_clearcoat` (second specular lobe — IMPORT_FIDELITY Deferred #1,
  trigger fired by the AMG's paint), `KHR_materials_specular` + `KHR_materials_ior`
  (map to F0 scale — parse already on). Everything else (sheen, iridescence,
  anisotropy-the-extension, volume, Draco, KTX2, meshopt) stays a report line and a
  named `xfail` entry in the conformance manifest — visible, counted, deferred with
  triggers in §8.
- **D6 — HDRI environments via a new `node.hdri_source` primitive,** IoBridge-shaped
  like `gltf_texture_source` (background decode thread → `Rgba16Float` upload →
  stretch-blit into its slot; mipmapped output like F-P6). Enables the `exr` feature
  of the existing `image` dep — no new crate. The import card's Environment section
  gains `env_mode` (enum: Softbox | HDRI) and a `hdri_file` Browse string binding;
  softbox stays the default (Peter's black-void aesthetic is the product look; HDRI
  is the fidelity option). The bake/prefilter path is UNCHANGED — `render_scene`
  already consumes any equirect texture on its `envmap` port. Rejected: baking HDRIs
  into the softbox primitive (mixes a file IO bridge into a pure-GPU bake node —
  violates the atom rule).
- **D7 — Anisotropic filtering: one new field, defaulted inert.**
  `GpuSamplerDesc.max_anisotropy: u32`, default `1` (byte-identical everywhere —
  every existing `..Default::default()` and struct-literal site keeps its behavior).
  `render_scene::ensure_sampler` sets `8`. Mip UV-island bleed (the green
  contamination seen 2026-07-15) is NOT addressed in v1 — Deferred #3, trigger
  stated there. (Correction, same day: the dominant 'smear' was the clamp-V wrap
  bug, fixed at `85b5bb9d` before this doc executes — aniso's remaining job is
  ordinary glancing-angle sharpness, priced accordingly.)
- **D8 — The emissive path is certified working; no phase may "fix" it.** Verified
  2026-07-15 value-level at every hop (audit row). What LOOKED broken was strip
  specular ~10× brighter than the emissive term pre-F-P6, plus probe-camera
  geometry. G-P1's EmissiveStrength conformance case pins the truth mechanically.
  An executor who believes emissive is broken must re-read this decision and run
  the conformance case before touching anything.

## 3. Committed shapes

New/changed signatures — transcribe, don't reinterpret:

```rust
// crates/manifold-gpu/src/types.rs — GpuSamplerDesc grows ONE field (D7)
pub struct GpuSamplerDesc {
    pub min_filter: GpuFilterMode,
    pub mag_filter: GpuFilterMode,
    pub mip_filter: GpuFilterMode,
    pub address_mode_u: GpuAddressMode,
    pub address_mode_v: GpuAddressMode,
    pub address_mode_w: GpuAddressMode,
    pub compare: Option<GpuCompareFunction>,
    /// Max anisotropic sample count. 1 = isotropic (the default; byte-identical
    /// to pre-field behavior). Metal: `setMaxAnisotropy` (1..=16).
    pub max_anisotropy: u32,
}

// crates/manifold-renderer/src/headless_readback.rs — the ONE shared transform (D2)
/// Read back an Rgba16Float target and encode to 8-bit sRGB PNG bytes exactly
/// the way the app presents HDR output. THE only tonemap in headless tooling —
/// render_import, generate_preset_thumbnails, and conformance tests all call this.
pub fn readback_to_srgb_png(
    device: &GpuDevice,
    texture: &GpuTexture,
    width: u32,
    height: u32,
) -> Vec<u8>;

// crates/manifold-renderer/src/bin/render_import.rs — CLI (D2)
// usage: render-import <file.glb> [--size WxH] [--out PATH] [--param id=value ...]
//        [--orbit R] [--tilt R] [--frames-max N]
// exit 0 = PNG written after convergence; exit 2 = never converged (prints last
// non-black fraction); exit 3 = import error (prints ImportReport + error).

// conformance manifest — tests/fixtures/gltf/khronos/manifest.json (D1/D3)
// [{ "asset": "EmissiveStrengthTest.glb",
//    "checks": [{ "kind": "lights_off_nonblack_min", "value": 0.002 },
//               { "kind": "golden", "file": "goldens/emissive_strength.png",
//                 "mean_abs_tol": 2.0 }],
//    "status": "expect_pass" | "xfail:<reason>" }]
```

`node.hdri_source` (D6): copy `gltf_texture_source.rs` wholesale as the shape —
same extra_fields (pending_load/pending_upload/last_mip_identity), same
`output_mipmapped`, same blit, same `boundary_reason: IoBridge`; params are `path`
(String via stringBindings Browse), `width`/`height` (Int, default 2048/1024).
Decode: `image::open` with the `exr` feature → `Rgb32F` → f16 upload. sRGB handling:
EXR is linear — upload `Rgba16Float` directly, no color_space param at all.

## 4. Invariants & enforcement

| Invariant | Enforcement (machine check, by name) |
|---|---|
| Nothing in a glb is silently dropped: every unmapped feature/material/texture is a report line | existing importer unit test extended in G-P2: synthetic 100-material fixture → `report_lines` + object count asserts (`gltf_import::tests::over_cap_asset_imports_one_to_one`) |
| Import is 1:1 — object count == material count with geometry, no truncation | same test; plus negative gate `rg -n "dropped_over_cap" crates/` → **zero hits** after G-P2 |
| One display transform for all headless tooling | negative gate in G-P1: `rg -n "fn tonemap|/ \(1\.0 \+" crates/manifold-renderer/src/bin/` → zero hits outside `headless_readback.rs`; thumbnails + render_import both call `readback_to_srgb_png` (compile-time: the fn is the only pub readback) |
| `max_anisotropy: 1` is byte-identical to pre-field behavior | G-P3 gpu proof `sampler_aniso_one_is_byte_identical` (render same scene with explicit 1 vs a build prior — implemented as: field default renders byte-equal to a desc built without touching the field) |
| Emissive stays working (D8) | conformance case `EmissiveStrengthTest` `lights_off_nonblack_min` runs in every conformance sweep |
| Conformance goldens change only deliberately | golden update requires `UPDATE_CONFORMANCE_GOLDENS=1`; the test fails (not regenerates) on mismatch otherwise |
| HDRI file never blocks the content thread | `node.hdri_source` reuses the background-thread decode shape; G-P6 gate greps the primitive for `std::fs`/decode calls outside the spawned thread: `rg -n "image::open|std::fs" <file>` must hit only inside the `thread::spawn` closure |

## 5. Phasing — Sonnet→Sonnet, one session per phase

Ordering rationale: G-P1 first because it is the vertical slice AND the oracle every
later phase gates against. G-P2 (cap) before extensions because 1:1 import changes
object counts that goldens would otherwise churn on. Every phase: clippy scoped
`-p <touched crates>`; full workspace sweep only at landing in the main checkout.
Every phase report carries `Shortcuts taken:` and `Demo artifact:` per standard §8.7.

### G-P1 — the conformance harness (vertical slice) — SHIPPED 2026-07-15 (`909976d2`)

- **Entry state:** `git log --oneline -1` contains `44b921cf` in ancestry
  (`git merge-base --is-ancestor 44b921cf HEAD`); `cargo run -p manifold-renderer
  --bin render_generator_preset -- --help` exits 0 (precedent binary alive);
  `tests/fixtures/gltf/DamagedHelmet.glb` exists.
- **Read-back:** this doc §1–§4 whole; `render_generator_preset.rs` end-to-end;
  `generate_preset_thumbnails.rs` (its readback is what moves into
  `headless_readback.rs`); `gltf_import.rs` test
  `damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate` (the
  convergence-poll pattern to reuse — byte-stable + non-black floor, BUG-100
  comment explains why both). Restate: D2 (shared transform, no local tonemap),
  D3 (no Khronos pixel-matching), D8 (emissive is not broken).
- **Deliverables:** `headless_readback.rs` (extracted, thumbnails migrated onto it);
  `bin/render_import.rs` per §3 CLI; `scripts/fetch-gltf-conformance.sh` (pinned
  Khronos commit, downloads ONLY the assets named in the manifest, ~10 files);
  `tests/fixtures/gltf/khronos/manifest.json` v1 with exactly these assets:
  `MetalRoughSpheres`, `EmissiveStrengthTest`, `TextureTransformTest`,
  `ClearCoatTest`, `AlphaBlendModeTest`, `NormalTangentMirrorTest`,
  `SpecularTest`, `TextureSettingsTest`.
  **Corrected 2026-07-15 during G-P1 execution** (the original bullet here
  grouped `expect_pass`/`xfail` by list position — "the first four
  `expect_pass`... the rest `xfail:<phase>`" — which put `TextureTransformTest`
  and `ClearCoatTest` in the pass bucket and `AlphaBlendModeTest`/
  `NormalTangentMirrorTest` in the fail bucket, directly contradicting this
  doc's own G-P4 entry state ("`TextureTransformTest` and `SpecularTest`
  currently xfail") and G-P5 entry state ("`ClearCoatTest` xfail"), and
  contradicting D5's own phasing — `KHR_materials_clearcoat`/
  `KHR_texture_transform`/`specular+ior` are explicitly *not yet mapped*
  (G-P4/G-P5), while base PBR/alpha-blend/normal-mapping are already shipped.
  Resolved by running all seven fetchable assets through `render-import` and
  reading the PNGs — the correct split, by whether a D5-deferred extension
  gates the asset:
  `expect_pass` = `MetalRoughSpheres`, `EmissiveStrengthTest`,
  `AlphaBlendModeTest`, `NormalTangentMirrorTest` (no deferred extension;
  all four render cleanly, non-degenerate, this session);
  `xfail:G-P4` = `TextureTransformTest` (also has no `glTF-Binary` variant in
  the pinned Khronos commit — `Models/TextureTransformTest/glTF` only, JSON +
  side-car `.bin`/textures; the fetch script skips it and the conformance
  test treats it as not-yet-fetchable, same as any other missing fixture),
  `SpecularTest`; `xfail:G-P5` = `ClearCoatTest`. `TextureSettingsTest`
  renders non-degenerate but exercises per-texture sampler wrap/filter
  settings that the current importer cannot honor (BUG-164, logged this
  session: every material map shares one hardcoded REPEAT sampler) — no
  future phase in this doc currently owns that fix, so it is `xfail:BUG-164`
  pending a phase assignment.
  Conformance test module `crates/manifold-renderer/tests/glb_conformance.rs`
  (skip-if-absent, table-driven from the manifest); goldens for the
  `expect_pass` set.
- **Gate (positive):** `bash scripts/fetch-gltf-conformance.sh && cargo test -p
  manifold-renderer --features gpu-proofs --test glb_conformance --
  --test-threads=1` → every `expect_pass` green, every `xfail` reported as
  xfail (not silently skipped); `cargo run -p manifold-renderer --bin
  render_import -- tests/fixtures/gltf/DamagedHelmet.glb --out /tmp/helmet.png`
  exits 0. **Held-out input:** the orchestrator (not the worker) additionally runs
  `render_import` on ONE Khronos asset absent from the manifest and confirms exit
  0 or a clean exit-3 report — never a panic.
- **Gate (negative):** the display-transform rg gate (§4); `rg -n "reinhard|/ \(1\.0
  \+ v\)" crates/manifold-renderer/src/bin/ crates/manifold-renderer/tests/` →
  zero hits.
- **Acceptance demo (L2):** `/tmp/helmet.png` + the conformance run's summary table
  pasted in the report. The orchestrator LOOKS at the helmet PNG.
- **Forbidden moves:** writing any tonemap in the test/bin files (call the shared
  fn); pixel-comparing against Khronos-published renders (D3); vendoring assets
  into git (D1); marking a failing `expect_pass` as `xfail` to get green — a
  failing expect_pass is an escalation with the diff attached; touching
  `render_scene`/importer code (this phase builds the oracle, not the fixes).
- **Test scope:** focused (`-p manifold-renderer`); gpu-proofs for the conformance
  binary only.

### G-P2 — 1:1 import (the cap dies) — SHIPPED 2026-07-15 (`909976d2`)

- **Entry state:** G-P1 landed (`cargo test ... --test glb_conformance` runs);
  re-verify anchors: `rg -n "OBJECT_SLIDER_MAX" crates/manifold-renderer/src/` —
  if the constant moved since 2026-07-15, stop and re-derive.
- **Read-back:** D4 verbatim; BUG-163 in `docs/BUG_BACKLOG.md`; `gltf_import.rs:338-380`
  (triage-and-drop being deleted); `render_scene.rs:97,546`. Restate the seam:
  clamp → 1024 loud-error bound; importer wires all; card curates 16.
- **Deliverables:** renderer clamp change + import-time error path (>1024 objects →
  `Err` with count, never truncation); importer 1:1 wiring; card curation (largest
  16 by vertex count get sliders — pure `card_params` change); `ImportReport`
  loses `dropped_over_cap` (compiler-driven — delete the field first, fix every
  red site); synthetic 100-material fixture generator (build the glb in the test
  with the `gltf` crate's writer or raw JSON — committed as a small builder fn,
  not a binary asset) + `over_cap_asset_imports_one_to_one` test; AMG conformance:
  with the untracked AMG present, `render_import` on it must report
  `object_count == 78`.
- **Gate:** named tests green; negative: `rg -n "dropped_over_cap" crates/` zero
  hits; goldens: DamagedHelmet golden UNCHANGED (1 material — proves no collateral);
  a fresh AMG render (if present) attached to the report.
- **Acceptance demo (L2):** AMG PNG before/after, side by side. **Held-out:** one
  multi-material Khronos asset (e.g. `SciFiHelmet` or suite equivalent) renders with
  object_count == its material count.
- **Round-trip gate:** import → save `.manifold` → reload → object count and card
  params identical (the standard §5 round-trip rule; imports serialize).
- **Performer gesture:** drop a 78-material car into a set; every panel exists;
  the card shows 16 sliders, not 78.
- **Forbidden moves:** raising 64 to another silent number; merging meshes;
  keeping `dropped_over_cap` "for compatibility"; per-object slider explosion
  (curation cap is UI-only and stays).
- **Test scope:** focused + the conformance sweep.

### G-P3 — anisotropic filtering — SHIPPED 2026-07-15 (session 2)

- **Entry state:** G-P1 landed. Re-verify: `rg -n "max_anisotropy" crates/` → zero
  hits (else stop: someone built it).
- **Read-back:** D7; `types.rs:144` (desc + its `Default`); `metal/device.rs:293`
  (`create_sampler` — where `setMaxAnisotropy` lands); `render_scene.rs`
  `ensure_sampler`. Restate: default 1, render_scene 8, everything else untouched.
- **Deliverables:** the field + Metal mapping + `Default` impl; `ensure_sampler`
  → 8; Vulkan stub notes the field (`vulkan/` module compiles, field ignored with
  a `// VULKAN_BACKEND_DESIGN:` comment naming the tracked gap); gpu proof
  `sampler_aniso_one_is_byte_identical`; gpu proof
  `aniso_sharpens_grazing_minification` (render a striped texture on a grazing
  quad at aniso 1 vs 8 — assert high-frequency energy increases, the F-P3
  numeric-not-look style).
- **Gate:** both proofs green + full render_scene gpu suite green unmodified +
  conformance sweep green with goldens updated ONLY for material-map assets (the
  expected sharpening — the golden diff is attached to the report and reviewed by
  the orchestrator, not waved through).
- **Acceptance demo (L2):** DamagedHelmet via `render_import` at a grazing camera
  (`--orbit 2.4`), aniso off vs on, side by side.
- **Forbidden moves:** touching mip generation (bleed is Deferred #3); enabling
  aniso on samplers other than render_scene's material sampler "while at it".
- **Test scope:** focused; gpu-proofs render_scene + the two new proofs.

### G-P4 — KHR_texture_transform + specular/ior mapping — SHIPPED 2026-07-15 (session 2)

- **Entry state:** G-P1+G-P2 landed; conformance sweep runs; `TextureTransformTest`
  and `SpecularTest` currently xfail.
- **Read-back:** D5; the gltf crate's `texture().texture_transform()` API (read the
  crate docs page for the exact accessor — ⚠ VERIFY-AT-IMPL: `cargo doc -p gltf
  --no-deps --open` or docs.rs for the pinned version); `render_scene.wgsl`
  resolve fns (uv comes in per-fragment; the transform is per-map static). Restate:
  transform = per-map `[offset, rotation, scale]` folded into a 2×3 affine uniform.
- **Deliverables:** loader carries per-map transform; shader ABI: one `vec4 +
  vec4` per map family or a packed `mat2x3` per the uniform-alignment rules in
  `docs/MANIFOLD_GPU_ARCHITECTURE.md` (read before choosing packing — the doc's
  alignment table is binding); resolve fns apply `uv' = M * uv` when the flag is
  set; specular/ior → F0 scale in `fs_pbr` (**corrected 2026-07-15 during G-P4
  execution:** this brief originally wrote `F0 = 0.16 * ((ior-1)/(ior+1))^2 *
  specular_color_factor`, which is NOT the spec — the `0.16 *` is spurious
  (default IOR 1.5 would give F0≈0.0064 instead of 0.04) and `specularFactor`
  was missing. The Khronos KHR_materials_specular README formula, implemented:
  `dielectric_f0 = min(((ior-1)/(ior+1))^2 * specularColorFactor, 1.0) *
  specularFactor`, dielectrics only; defaults proven inert by byte-stable
  goldens); conformance flips
  `TextureTransformTest` + `SpecularTest` to `expect_pass` with goldens.
- **Gate:** those two assets pass their numeric checks; all previously-passing
  conformance cases byte-stable (goldens unchanged — transforms default to
  identity); render_scene gpu suite green.
- **Acceptance demo (L2):** TextureTransformTest PNG — the Khronos asset is
  literally a labeled grid of correct/incorrect transform tiles; the orchestrator
  reads the labels.
- **Forbidden moves:** applying the transform CPU-side by rewriting UVs in the
  vertex buffer (breaks shared meshes); a per-frame matrix inversion; skipping the
  uniform-alignment doc.
- **Test scope:** focused + gpu-proofs render_scene + conformance sweep.

### G-P5 — clearcoat lobe — SHIPPED 2026-07-15 (session 2)

- **Entry state:** G-P4 landed (uniform packing precedent set); `ClearCoatTest`
  xfail.
- **Read-back:** D5; IMPORT_FIDELITY Deferred #1 (the original pricing);
  `fs_pbr` in `render_scene.wgsl` (the existing GGX terms to reuse); MATERIAL
  M §7 "new fields, defaulted" seam. Restate: second specular lobe, NOT a second
  material system.
- **Deliverables:** `Material` grows `clearcoat: f32` + `clearcoat_roughness: f32`
  (defaulted 0.0 — every existing constructor site unchanged by `..` or explicit
  zeros; compiler-driven); loader maps the extension factors (textures for
  clearcoat stay report lines — factor-only v1, stated in the report); `fs_pbr`
  adds the standard second GGX lobe (same D/G/F helpers, fixed F0=0.04, energy
  compensation `base *= 1 - Fc`); importer maps extension → params; conformance
  flips `ClearCoatTest` to expect_pass.
- **Gate:** ClearCoatTest numeric check (clearcoat=1 sphere has brighter specular
  peak than clearcoat=0 — the asset is built for exactly this comparison) +
  goldens for prior cases unchanged (defaults inert) + full render_scene gpu
  suite + workspace sweep at landing (shader ABI = infra per the F-P1 precedent).
- **Acceptance demo (L2):** ClearCoatTest PNG + the AMG (if present) before/after.
- **Forbidden moves:** clearcoat textures in v1 (factor only — report line);
  touching the Blend/glass pass; "improving" the base BRDF while in the file.
- **Test scope:** focused + gpu suite; workspace sweep at landing.

### G-P6 — `node.hdri_source` — SHIPPED 2026-07-15 (session 3, `017e1e41`)

- **Entry state:** G-P1 landed. `kloppenheim_07_puresky_4k.exr` present locally
  (skip-demo-if-absent, same pattern) — fetch line added to the conformance
  script (Poly Haven CC0 direct URL).
- **Read-back:** D6 verbatim; `gltf_texture_source.rs` END TO END (it is the
  template — extra_fields, background thread, mip regeneration, step numbering);
  the F-P6 section of IMPORT_FIDELITY (mip contract). Restate: EXR is linear, no
  color_space param; output mipmapped.
- **Deliverables:** `image` dep gains `exr` feature (one Cargo.toml line — this is
  the approved dependency change, no other); `primitives/hdri_source.rs` per §3;
  registry entry + catalog regen (`cargo run -p manifold-renderer --bin
  gen_node_catalog`); importer `env_mode` enum param + `hdri_file` string binding
  + card wiring (Environment section); prefilter cost measurement at 4096×2048
  reported as a number (the F-P1 gate pattern) — if the first-frame convolution
  exceeds 10ms, drop the node's default width/height to 2048×1024 and state it.
- **Gate:** unit tests (params/ports/skip-if-absent decode test with a tiny
  committed 64×32 EXR fixture — generate it in the test with the `exr` crate via
  `image`, don't commit a binary); the content-thread grep gate (§4); conformance
  sweep unchanged (HDRI is opt-in); `check-presets` green.
- **Acceptance demo (L2):** DamagedHelmet via `render_import` with
  `--param env_mode=1 --param hdri_file=<kloppenheim path>` — the helmet lit by
  real sky, PNG in the report. **Performer gesture:** swap the environment file
  live from the card's Browse field; the reflections follow within a second.
- **Forbidden moves:** decoding on the content thread; a color_space param
  (EXR is linear, full stop); baking HDRI support into `bake_equirect_envmap`;
  resizing/re-encoding the EXR on CPU beyond the decode.
- **Test scope:** focused + gpu-proofs for the new primitive's decode/upload test.

### G-P7 — burn-down and certification

- **Entry state:** G-P1–G-P6 landed; conformance sweep green on its expect_pass
  set.
- **Read-back:** the manifest; this doc §2 D3/D5; `docs/VERIFICATION_DEBT.md`.
- **Deliverables:** manifest grows to the FULL Khronos suite glb list (script
  fetches all); every asset classified in the manifest: `expect_pass` (with
  checks+golden), or `xfail:<deferred-item>` pointing at a §8 entry — **no third
  state**; a `docs/GLB_CONFORMANCE_STATUS.md` generated table (asset × status ×
  gap) committed as the certification record; BUG_BACKLOG sweep: BUG-163 status
  → FIXED (G-P2 reference).
- **Gate:** `cargo test ... --test glb_conformance` green across the full
  manifest; the status doc's xfail count is REPORTED as a number in the landing
  report; zero assets in "unclassified".
- **Acceptance demo (L2):** the status table itself + three PNGs chosen by the
  orchestrator from assets never named in any brief (held-out spirit at suite
  scale).
- **Forbidden moves:** deleting a failing asset from the manifest; classifying a
  renderable-but-ugly result as xfail without a named deferred item; "close
  enough" golden tolerances above the stated 2/255.
- **Test scope:** conformance sweep + workspace sweep at landing.

## 6. Decided — do not reopen

1. Ground truth = Khronos suite, fetched not vendored; skip-if-absent (D1).
2. One shared display transform; harness-local tonemaps are forbidden (D2).
3. No pixel-matching against Khronos's own renders; numeric feature asserts +
   self-goldens at 2/255 (D3).
4. The cap is deleted, not raised; card curation is UI-only; >1024 objects errors
   loudly (D4).
5. v1 extensions: emissive_strength, texture_transform, clearcoat (factors),
   specular+ior. Nothing else (D5).
6. HDRI = new IoBridge primitive, `exr` feature of the existing image dep, linear,
   softbox stays default (D6).
7. Anisotropy field defaults 1; only render_scene's material sampler uses 8 (D7).
8. **Emissive works.** Certified 2026-07-15; do not "fix" it (D8).
9. IMPORT_FIDELITY's decisions (D1–D9 there) all stand; this doc builds on top.

## 7. Deferred (with triggers)

1. **Mip UV-island bleed padding** — trigger: after G-P3 lands, a hero asset still
   shows cross-island color contamination in its conformance golden. Fix shape:
   island-aware dilation pass at upload time in `gltf_texture_source`.
2. **Clearcoat textures** (map-driven coat/roughness) — trigger: an asset whose
   coat VARIES visibly (factor-only renders it uniform); the report line names it.
3. **Sheen / iridescence / volume / transmission-roughness extensions** — trigger:
   an asset in Peter's actual library uses one (the report line + xfail entry make
   it visible the day it happens).
4. **Draco / KTX2 / meshopt compressed assets** — trigger: same as #3. These are
   decode dependencies, priced separately when fired.
5. **Real HDRI *background* rendering** (drawing the env behind the model) —
   trigger: Peter asks for it; today the black void is the product look (his D7
   quote in IMPORT_FIDELITY stands).
6. **Vulkan aniso mapping** — trigger: VULKAN_BACKEND_DESIGN execution begins; the
   field lands there with a comment naming this doc.
7. **Animation / skinning / morph targets** — out of scope for this doc entirely;
   owned by IMPORT_DESIGN P1-remaining (§8 there). Rendering conformance only here.
