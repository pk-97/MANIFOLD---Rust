# glTF Material Extensions — transmission, volume, sheen, iridescence, anisotropy, dispersion

**Status:** IN PROGRESS 2026-09-26 — BUG-5l4o correctness review reopened the E1–E6 implementation. Section 7 supersedes the affected shading and texture assumptions. Focused numeric proofs are separate from the existing conformance manifest; its classifications do not establish physical accuracy or Peter's visual acceptance.
**Prerequisites:** GLB_XFAIL_BURNDOWN_DESIGN.md P2 (slice-based import with our extension gate) — the gate's supported-list grows per phase here. MATERIAL_SYSTEM_DESIGN.md (SHIPPED M1–M6) is the material contract this extends.
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md section 5 (Phase briefs)–section 6 (Seam briefs — refactors and API changes) before starting any phase.

Peter's directive (2026-07-16): *"a third doc for all the extensions would be useful too."* This owns the **49 material-extension xfails** in `docs/GLB_CONFORMANCE_STATUS.md` — the largest remaining block between 69/148 and full certification (original scope at design time; E6's certification pass resolved all but 7 of them — see the Status header for the final 148-asset arithmetic). It is real shading work (new BRDF lobes and a transmission pass), not import plumbing.

Instrument frame: these extensions are the difference between "store-page preview" and "grey approximation" for exactly the asset classes Peter's EP aesthetic uses — **glass and translucency (vases, broken windows, amber, plants in glass) are transmission+volume; fabric and velvet are sheen; petrol-sheen and abalone are iridescence**. The flowers/nature Interim-EP direction makes transmission the highest-value family by a wide margin.

## 1. Audit — current shading state (verified 2026-07-16; RE-DERIVE at execution — this section WILL be stale)

Re-derivation commands, run before any phase:
```
rg -n 'clearcoat|specular_factor|ior|transmission' crates/manifold-renderer/src/node_graph/primitives/render_scene.rs | head -40
rg -n 'transmission|sheen|iridescence|volume|anisotrop|dispersion' crates/manifold-renderer/src/node_graph/gltf_load.rs
python3 - <<'EOF'  # current xfail families
import re; t=open('docs/GLB_CONFORMANCE_STATUS.md').read()
print(re.findall(r'\*\*Gap:\*\* unsupported material extension.*?(?=\*\*Gap|\Z)', t, re.S)[0][:2000])
EOF
```

Snapshot (2026-07-16): the PBR shader is single-scatter GGX metal-rough + a second GGX clearcoat lobe (G-P5), `specular_factor`/`ior` in reserved uniform slots (`render_scene.rs:263,307` — G-P4). Transmission is parsed (`gltf_load.rs:443-447`) but approximated as alpha: `alpha = base_color.a * (1 - transmission_factor)` (`gltf_import.rs:664`) — report-only fidelity, no refraction, no roughness blur. Environment: `node.hdri_source` + softbox dome fill (G-P6 / F-P7). No sheen, iridescence, volume, anisotropy, or dispersion parsing anywhere (`rg` above returns zero hits for those terms in the import path today).

The 49 assets by family (from the status doc; counts overlap — many assets use several extensions; re-derive at execution):
transmission/diffuse-transmission/volume/attenuation ≈ 16 · sheen ≈ 5 · iridescence ≈ 5 · anisotropy ≈ 6 · dispersion ≈ 3 · IOR-grid ≈ 2 · showcase multi-extension (ToyCar, CarConcept, ABeautifulGame, SunglassesKhronos, USDShaderBall, ChronographWatch, …) ≈ 12 — the showcases only pass once ALL their extensions land, so they gate the final phase, not the per-family ones.

## 2. Decisions

- **D1 — One shader, additive lobes, full spec surface per family.** Each family lands as an addition to the existing render_scene fragment shading, gated per-object by its uniform factors (zero factor = today's code path bit-exact). **A phase ships its family's complete spec surface — factors AND the family's extension textures — in the same phase.** (REVISED by Peter 2026-07-16; the original "factor-only first, textures on a real-asset trigger" doctrine — the clearcoat precedent — is REVOKED: "we shouldn't wait until we hit an error if the technical work is straightforward and trivial." The texture plumbing already exists for base PBR maps; per-family textures are a sampler + mix into the family's lobe. See the `full-spec-support-over-trigger-deferral` memory.) Rejected: a separate "advanced PBR" shader variant, because two shading paths for one material model is the parallel-old-path forbidden move at shader scale.
- **D2 — Uniform growth is a real layout change, done once.** Clearcoat rode the last reserved slots; there are none left (⚠ VERIFY-AT-IMPL via the re-derivation rg). Phase E1 grows the per-object material uniform by one aligned block sized for ALL families in this doc (sheen color+roughness, iridescence ior/thickness, anisotropy strength+rotation, transmission+volume params, dispersion) in a single migration, so later phases only fill fields. Respect `feedback_naga_uniform_size_rule` + `feedback_wgsl_vec3_alignment` (memories); GPU parity tests are the gate. Rejected: growing the uniform per phase, because five layout migrations × golden re-baselines is five times the regression risk.
- **D3 — Transmission is a screen-space refraction pass, not ray tracing.** The house-standard real-time approach: opaque pass renders to an intermediate; transmissive objects sample it (roughness-blurred via the existing mip chain infra from F-P6) with IOR-based refraction offset; volume/attenuation (Beer–Lambert from `attenuationColor`/`attenuationDistance`/`thicknessFactor`) tints it; dispersion = per-channel IOR spread on the same sample. This reuses the G-Buffer/CINEMATIC_POST infra (⚠ VERIFY-AT-IMPL: whether render_scene's pass structure exposes an opaque-resolved texture — re-derive from `docs/FREEZE_COMPILER_MAP.md` + render_scene's pass code; if not, the intermediate is this phase's real work and the phase brief must be split by the executor's orchestrator, not improvised). Rejected: alpha-blend approximation kept permanently, because glass that doesn't refract or blur reads as cellophane on stage — the current approximation is explicitly the thing this doc removes.
- **D4 — Family order is EP-value order: transmission+volume → sheen → iridescence → anisotropy → dispersion.** Peter's release focus decides this, not spec completeness.
- **D5 — Each family's acceptance is its Khronos Compare asset.** `CompareTransmission`, `CompareSheen`, `CompareIridescence`, `CompareAnisotropy`, `CompareDispersion`, `CompareIor` are side-by-side fixtures designed for exactly this; the golden + a region check (extension half must differ from the base half in the documented direction) is the mechanical gate. Showcase assets certify only in the final phase.

## 3. Phasing (briefs at conformance level — each phase re-derives its inventory; one phase = one session)

- **E1 — Uniform layout growth + parse plumbing.** All families' factors parsed in `gltf_load.rs` (typed accessors where 1.4.1 has them, raw-JSON sniff per the clearcoat precedent where not — ⚠ VERIFY-AT-IMPL per family), carried through `GltfMaterialInfo` → uniform block. Gate: GPU parity suite green; all 56+ passing goldens byte-stable (zero-factor = bit-exact is D1's promise, this gate proves it); no visual change anywhere.
- **E2 — Transmission + volume + attenuation (the glass phase).** D3's refraction pass, factor-first then `transmissionTexture`. Gate: `CompareTransmission`, `CompareVolume`, `AttenuationTest`, `TransmissionRoughnessTest` flip to expect_pass; held-out: `GlassVaseFlowers.glb` (EP-adjacent: flowers in glass) reviewed as PNG by the orchestrator. Instrument line: a glass object over a live video layer must show the layer through it, refracted — that's the stage payoff and the demo.
- **E3 — Sheen.** Charlie/Ashikhmin sheen lobe per the glTF spec reference, including `sheenColorTexture` + `sheenRoughnessTexture` (D1 revised: full spec surface). Gate: `CompareSheen`, `SheenTestGrid` + goldens; held-out `SheenChair` or `GlamVelvetSofa`; at least one gate asset must exercise a sheen texture (pick from the manifest at re-derivation — if none does, say so in the landing note rather than skipping the texture path).
- **E4 — Iridescence.** Thin-film Fresnel modulation per spec, including `iridescenceTexture` + `iridescenceThicknessTexture` (the importer already detects both — `gltf_load.rs` `has_*` flags). Gate: `CompareIridescence`, `IridescenceSuzanne` + held-out `IridescenceAbalone`; same texture-coverage rule as E3.
- **E5 — Anisotropy.** Tangent-space GGX stretch. **Tangent question RESOLVED (2026-07-16 pre-execution audit):** imported meshes carry NO tangent attribute — `MeshVertex` (`generators/mesh_common.rs`) is a fixed 48-byte position/normal/uv layout with a size assert and a `MESH_VERTEX_SPECS` channel signature; normal mapping instead reconstructs a cotangent frame in-shader from screen-space derivatives (`render_scene.wgsl`, D3/F-P2). **Decision: E5 reuses that cotangent frame as the anisotropy tangent basis**, rotated by `anisotropyRotation` + the anisotropy texture per spec. Do NOT import glTF `TANGENT` attributes or grow `MeshVertex` in this phase — that is a vertex-layout project rippling through every mesh atom, the channel system, and codegen (section 5 Deferred). If the numeric gate fails specifically from tangent-frame mismatch (UV seams / degenerate UVs on a Compare asset), write the finding into the Status line and stop — do not improvise a layout change overnight. Gate: `CompareAnisotropy`, `AnisotropyStrengthTest`/`RotationTest`; held-out `AnisotropyBarnLamp`. **Superseded 2026-08-01 (BUG-wfxe — gltf-tangent-import):** the vertex-layout project landed — `MeshVertex` is 64 bytes with authored `TANGENT` imported end-to-end; anisotropy and normal mapping now use the authored frame when present, the cotangent frame as fallback.
- **E6 — Dispersion + texture-completion sweep + certification.** Per-channel IOR on E2's pass (dispersion defines no texture in the spec). Then the **texture-completion sweep** (D1 revised): audit every already-shipped family for factor-only gaps and close them — known candidates at authoring time: `clearcoatTexture`/`clearcoatRoughnessTexture`/`clearcoatNormalTexture` (G-P5 landed factors-only), `specularTexture`/`specularColorTexture`, `transmissionTexture` if E2 landed factor-only, volume `thicknessTexture`; re-derive the actual list by diffing `gltf_load.rs`'s `has_*_texture` detection flags against what the shader samples. Then the multi-extension showcases (`ToyCar`, `ABeautifulGame`, `SunglassesKhronos`, …) certified, manifest re-classified, `scripts/gen_glb_conformance_status.py` regenerated, and the final pass/xfail arithmetic written into the status doc. Any showcase still failing gets a named xfail reason or a BUG entry — zero unclassified, same bar as G-P7.

Each phase: clippy scoped per CLAUDE.md; GPU suite (`cargo test -p manifold-renderer --features gpu-proofs`, render_scene-scoped) because every one touches the shader; landing batches 2–3 phases per GIT_TREE_DISCIPLINE section 2c; every landing reruns the status generator and updates this doc's Status line.

## 4. Decided — do not reopen
1. One shader, additive zero-default lobes; no advanced-PBR fork (D1).
2. One uniform migration up front, not per family (D2).
3. Transmission = screen-space refraction + Beer–Lambert volume; no ray tracing, no permanent alpha approximation (D3).
4. Family order is D4's; re-ordering requires Peter.

## 5. Deferred
- `KHR_materials_diffuse_transmission` full BTDF (the three DiffuseTransmission assets) → fold into E2 if the factor path covers the Compare asset; else its own follow-up phase — decided by E2's gate result, recorded there.
- ~~Extension **textures** beyond each family's factor path~~ — REVOKED 2026-07-16 by Peter (no trigger-waiting on trivial work); textures are now in-phase scope (D1 revised) with the E6 completion sweep catching already-shipped families.
- Spec-gloss specular tint (inherited pointer from GLB_XFAIL_BURNDOWN D2) — superseded by the RGB-preserving conversion in section 7.
- glTF `TANGENT` attribute import (grow `MeshVertex` past 48 bytes, or a separate tangent stream) → trigger: E5's numeric gate failing from tangent-frame mismatch, or authored assets where the cotangent-frame approximation visibly breaks (UV seams, mirrored UVs). A vertex-layout project: touches `MESH_VERTEX_SPECS`, every mesh atom, deform atoms, codegen — its own design pass, never an overnight improvisation (E5 audit, 2026-07-16). **LANDED 2026-08-01 as BUG-wfxe (gltf-tangent-import)** (64-byte `MeshVertex`, `tbn_for` authored-frame selector with the cotangent fallback, `NormalTangentMirrorTest` red proof).

## 6. As built (E1–E6, all SHIPPED 2026-07-16)

- **E1+E2** — uniform layout growth for all 5 families, opaque-resolve pass split,
  real transmission/volume refraction shading. 4 Compare assets certified.
- **E3** — Charlie sheen lobe (D_Charlie NDF × Ashikhmin visibility, direct +
  approximate IBL), full spec surface. Gap: no fetched asset exercises
  `sheenColorTexture`/`sheenRoughnessTexture` — wired, shader-validated,
  numerically unverified.
- **E4** — thin-film iridescence: Belcour/Barla Airy summation ported
  term-for-term, modifies base F0 for direct + IBL; both gate assets exercise the
  textures.
- **E5** — tangent-space anisotropic GGX (Burley D + Heitz height-correlated
  Smith V, Filament-style bent-normal IBL), guarded to never run at strength=0 so
  non-anisotropic materials stay byte-identical; reuses the screen-space cotangent
  frame — no `MeshVertex` growth (section 2's tangent decision).
- **E6** — `KHR_materials_dispersion` per-channel IOR spread on E2's transmission
  pass, zero-cost when `dispersion == 0`; texture-completion sweep wired the last
  seven factor-only texture slots end to end (clearcoat ×3 incl. a real second
  shading normal for the coat lobe, specular ×2, transmission, thickness) into the
  two reserved uniform `w` slots as bitmasks. Certification re-classified the
  deferred bucket by rendering each asset through the production import path:
  most flipped to real `expect_pass`; the rest carry precise per-asset xfail
  reasons in the manifest (parse gaps, webp, diffuse-transmission deferred per
  section 5). CompareSpecular/CompareVolume stay `expect_pass` and fail per
  BUG-185 (gltf-material-texture-slots) — deliberate, pending Peter's
  re-baselining.

## 7. Material fidelity corrections (2026-09-26)

This section is the current contract for the corrections under BUG-5l4o. The
dated E1–E6 record above describes the original implementation.

- All three colour renderers (`render_scene`, `render_mesh`, `render_copies`)
  use `RenderScene` material evaluation. Legacy signed world-normal and separate
  roughness/metallic maps retain their existing interpretation at the adapter.
- Metallic/roughness maps multiply their factors. Normal-map scale and AO
  strength are retained from import. UV0 and UV1, affine transforms, wrap modes,
  min/mag filters and explicit no-mip/nearest-mip/linear-mip choices travel with
  each texture family. Extension maps use manual sampling to stay within Metal's
  sampler limit. Sets above UV1 produce an import warning and use UV0.
- Specular weight controls F0 and F90. The diffuse energy split uses the largest
  Fresnel component. Clearcoat applies its Fresnel once and uses the geometric
  normal unless a coat map is present. Iridescence uses each direct light's V·H;
  its view reflectance is converted to the equivalent input for split-sum IBL.
- Anisotropy uses `alphaT = mix(roughness², 1, strength²)` and
  `alphaB = roughness²`. Its tangent frame follows the authored mesh tangent,
  with the normal texture's coordinates used for derivative reconstruction
  when authored tangents are absent. Environment anisotropy remains a bent-normal
  approximation.
- Sheen has a Charlie environment convolution and directional-energy LUT, with
  compensation of the underlying layer. Thin diffuse transmission has an
  independent colour and its two texture inputs, reduces front diffuse, and
  uses the opposite hemisphere. It is not a subsurface scattering model.
- Refraction uses a clamped scene sampler, an off-screen environment transition,
  and one exposure application. Each sorted transparent surface sees the colour
  already drawn behind it. Opaque-depth RT lighting never substitutes into a
  transparent surface at a different depth. Refraction still uses screen-space
  projection and object-level sorting; this does not provide arbitrary nested
  dielectric transport.
- Raster and RT transform normals by the inverse transpose; tangents use the
  model's linear transform and determinant handedness. RT base colour and MR
  samples multiply factors; secondary hits use normal mapping, specular weight
  and colour maps, and the material's dielectric F0/F90. Metals suppress diffuse
  energy; unlit hits terminate with their colour and emission. Primary reflection
  rays use anisotropic GGX with the actual hit instance's tangent frame.
- `MeshVertex` is now 80 bytes: position/normal, UV0/UV1, tangent and RGBA
  `COLOR_0`. Missing colours are white. Interpolation and deformation preserve
  colour; base colour and cutout alpha multiply it. Mesh decode caches use a new
  format/key version while HDRI caches retain their existing version.
  New imports explicitly enable `vertex_colors` on their geometry sources.
  The load-time upgrade enables varying colours on older imports. Constant
  colours already baked into saved material factors keep a white vertex stream,
  preserving the equivalent factor product without tinting twice.
- Legacy specular/glossiness import preserves diffuse and RGB specular factors
  and maps. Glossiness conversion computes `1 - factor * texture.a`. Previously
  imported graphs recover omitted settings and maps from their source model at
  load, including RGB specular data. Generated legacy defaults are repaired;
  edited values, custom wiring, transforms and animation remain authored.
- Imported punctual lights retain raw intensity, inverse-square falloff, optional
  finite range and spot cones. Existing authored lights keep legacy attenuation
  by default; their falloff control can select the physical mode.
- PBR materials expose both diffusion and homogeneous volumetric random-walk
  subsurface scattering, with shared colour/radius/phase/weight controls. See
  [SUBSURFACE_MATERIAL_DESIGN.md](SUBSURFACE_MATERIAL_DESIGN.md) for the boundary
  model, cost, geometry requirements and evidence.

Project loading upgrades embedded imports and graph overrides in memory, with
the changes persisted on the next normal save. Missing source assets and
ambiguous custom topology produce notices and remain retryable after repair.
Shader corrections apply to existing materials independently of this source-data
upgrade. SSS stays disabled until selected deliberately; its diffusion/random-walk
mode, weight, radius, colour, phase and sample controls share a dedicated
Subsurface inspector section. New texture controls use the existing Advanced
parameter surface.

Focused enforcement lives in `render_scene_pbr_fidelity`, `render_scene_glass`,
`render_legacy_parity`, the alpha-depth unit proofs, and the RT transmission
proofs. Their numeric assertions are L1 evidence. Updating MANIFOLD goldens is
not a replacement for those assertions or an independent reference comparison.

The RT path remains a hybrid approximation. Secondary hits do not evaluate
clearcoat, sheen, iridescence, SSS or the full Phong/Cel models; glass and Blend
surfaces are absent from the acceleration input. Ray-hit texture sampling uses
level zero and authored magnification/addressing, without ray-cone minification.
Environment anisotropy uses a bent normal; reflected surfaces use an environment
approximation rather than recursive specular transport. The material texture
table admits at most 64 unique textures and reports capacity failure explicitly.
These are capability limits, not full material parity or a path-tracing claim.
Arbitrary DCC shader networks, nested dielectric volumes and spectral transport
are outside the supported material model. Glass validation covers the established
sampler, exposure and composition defects. Peter withdrew the grid-artifact
report; no artifact search or reproduction is required.
