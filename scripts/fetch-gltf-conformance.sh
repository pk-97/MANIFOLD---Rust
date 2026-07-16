#!/bin/sh
#
# fetch-gltf-conformance.sh
#
# Downloads the Khronos glTF-Sample-Assets fixtures named in
# tests/fixtures/gltf/khronos/manifest.json, from ONE PINNED commit of the
# suite — never the manifest's own network resolution, never vendored into
# git (D1, docs/GLB_CONFORMANCE_DESIGN.md). Re-running is a safe no-op: an
# asset already present at the right size is left alone.
#
# tests/fixtures/gltf/khronos/ is gitignored (except manifest.json itself);
# `cargo test -p manifold-renderer --features gpu-proofs --test
# glb_conformance` skip-if-absents any asset this script hasn't fetched, so
# CI and a fresh worktree both stay offline-green (D1).
#
# Run: bash scripts/fetch-gltf-conformance.sh

set -eu

# Pinned 2026-07-15 — bump deliberately, never silently, if the suite adds a
# fixture this manifest needs. `git ls-remote
# https://github.com/KhronosGroup/glTF-Sample-Assets.git HEAD` to find a new
# one.
COMMIT="2bac6f8c57bf471df0d2a1e8a8ec023c7801dddf"
BASE_URL="https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/${COMMIT}/Models"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
OUT_DIR="${REPO_ROOT}/tests/fixtures/gltf/khronos"
mkdir -p "${OUT_DIR}"

# GLB_CONFORMANCE_DESIGN.md G-P7: every asset in the pinned Khronos commit
# that has a glTF-Binary variant (118 as of the pin below), EXCEPT
# TextureTransformTest — fetched separately below via its established
# gltf+sidecar handling (G-P4), since the pin ships it only as a multi-file
# `glTF` variant. The 30 assets with NO glTF-Binary variant at this pin
# (FlightHelmet, SciFiHelmet, Sponza, Cube, Triangle, ... — enumerated via
# the GitHub API tree at fetch-list generation time, G-P7) are NOT fetched;
# they carry an explicit `xfail:G-P7` manifest entry noting the absence
# rather than being silently missing (the forbidden third state).
ASSETS="
ABeautifulGame
AlphaBlendModeTest
AnimatedColorsCube
AnimatedMorphCube
AnimationPointerUVs
AnisotropyBarnLamp
AnisotropyDiscTest
AnisotropyRotationTest
AnisotropyStrengthTest
AntiqueCamera
AttenuationTest
Avocado
BarramundiFish
BoomBox
Box
BoxAnimated
BoxInterleaved
BoxTextured
BoxTexturedNonPowerOfTwo
BoxVertexColors
BrainStem
CarConcept
CarbonFibre
CesiumMan
CesiumMilkTruck
ChairDamaskPurplegold
ChronographWatch
ClearCoatCarPaint
ClearCoatTest
ClearcoatWicker
CommercialRefrigerator
CompareAlphaCoverage
CompareAmbientOcclusion
CompareAnisotropy
CompareBaseColor
CompareClearcoat
CompareDispersion
CompareEmissiveStrength
CompareIor
CompareIridescence
CompareMetallic
CompareNormal
CompareRoughness
CompareSheen
CompareSpecular
CompareTransmission
CompareVolume
Corset
CubeVisibility
DamagedHelmet
DiffuseTransmissionPlant
DiffuseTransmissionTeacup
DiffuseTransmissionTest
DirectionalLight
DispersionTest
DragonAttenuation
DragonDispersion
Duck
EmissiveStrengthTest
Fox
GlamVelvetSofa
GlassBrokenWindow
GlassHurricaneCandleHolder
GlassVaseFlowers
IORTestGrid
InterpolationTest
IridescenceAbalone
IridescenceLamp
IridescenceSuzanne
IridescentDishWithOlives
Lantern
LightVisibility
LightsPunctualLamp
MaterialsVariantsShoe
MetalRoughSpheres
MetalRoughSpheresNoTextures
MorphPrimitivesTest
MorphStressTest
MosquitoInAmber
MultiUVTest
NegativeScaleTest
NodePerformanceTest
NormalTangentMirrorTest
NormalTangentTest
OrientationTest
PlaysetLightTest
PointLightIntensityTest
PotOfCoals
PotOfCoalsAnimationPointer
RecursiveSkeletons
RiggedFigure
RiggedSimple
ScatteringSkull
SheenChair
SheenTestGrid
SheenWoodLeatherSofa
SimpleInstancing
SpecGlossVsMetalRough
SpecularSilkPouf
SpecularTest
SunglassesKhronos
TextureCoordinateTest
TextureEncodingTest
TextureLinearInterpolationTest
TextureSettingsTest
TextureTransformMultiTest
ToyCar
TransmissionOrderTest
TransmissionRoughnessTest
TransmissionTest
TransmissionThinwallTestGrid
USDShaderBallForGltf
Unicode❤♻Test
UnlitTest
VertexColorTest
VirtualCity
WaterBottle
XmpMetadataRoundedCube
"

fetched=0
skipped=0
for name in ${ASSETS}; do
    out="${OUT_DIR}/${name}.glb"
    if [ -s "${out}" ]; then
        echo "[fetch-gltf-conformance] already have ${name}.glb, skipping"
        skipped=$((skipped + 1))
        continue
    fi
    url="${BASE_URL}/${name}/glTF-Binary/${name}.glb"
    echo "[fetch-gltf-conformance] fetching ${name}.glb"
    if ! curl -sfL -o "${out}.tmp" "${url}"; then
        echo "[fetch-gltf-conformance] ERROR: failed to fetch ${url}" >&2
        rm -f "${out}.tmp"
        exit 1
    fi
    mv "${out}.tmp" "${out}"
    fetched=$((fetched + 1))
done

# TextureTransformTest — the pinned commit ships it ONLY as a multi-file
# `glTF` variant (JSON + side-car .bin + textures; no glTF-Binary folder at
# all, confirmed against the GitHub API at pin time). GLB_CONFORMANCE_DESIGN.md
# G-P4: fetch the whole `Models/TextureTransformTest/glTF/` directory into
# its own subfolder — `gltf::import(path)` (the production loader) natively
# resolves the .gltf's sidecar .bin/textures relative to the .gltf's own
# path, so this works with zero loader changes. File list enumerated from
# the GitHub API tree at the pinned commit (7 files, fixed set — no manifest
# to walk).
TT_DIR="${OUT_DIR}/TextureTransformTest"
mkdir -p "${TT_DIR}"
TT_BASE_URL="https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/${COMMIT}/Models/TextureTransformTest/glTF"
TT_FILES="
TextureTransformTest.gltf
TextureTransformTest.bin
Arrow.png
Correct.png
Error.png
NotSupported.png
UV.png
"
tt_fetched=0
tt_skipped=0
for f in ${TT_FILES}; do
    out="${TT_DIR}/${f}"
    if [ -s "${out}" ]; then
        echo "[fetch-gltf-conformance] already have TextureTransformTest/${f}, skipping"
        tt_skipped=$((tt_skipped + 1))
        continue
    fi
    echo "[fetch-gltf-conformance] fetching TextureTransformTest/${f}"
    if ! curl -sfL -o "${out}.tmp" "${TT_BASE_URL}/${f}"; then
        echo "[fetch-gltf-conformance] ERROR: failed to fetch ${TT_BASE_URL}/${f}" >&2
        rm -f "${out}.tmp"
        exit 1
    fi
    mv "${out}.tmp" "${out}"
    tt_fetched=$((tt_fetched + 1))
done


# G-P7 sidecar-fetch sweep — the 29 assets that at this pin have NO
# glTF-Binary variant but DO have a multi-file `glTF` (JSON + sidecar .bin +
# textures) variant, confirmed per-asset against the GitHub API tree at the
# pinned commit (none of the 29 are missing a fetchable variant entirely).
# Same handling as TextureTransformTest above (G-P4), generalized: one
# (asset, relative-file-path) table instead of 29 hand-written blocks. Some
# assets' files live in a subdirectory of their own `glTF/` folder (e.g.
# `textures/guides.png`, `EnvironmentTest_images/roughness_metallic_0.png`)
# — mkdir -p the parent before fetching. A few filenames contain literal
# spaces (Box With Spaces); the local path keeps the space, only the URL is
# percent-encoded.
GP7_BASE_URL="https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/${COMMIT}/Models"
gp7_fetched=0
gp7_skipped=0
gp7_last_asset=""
while IFS="$(printf '\t')" read -r asset file; do
    [ -z "${asset}" ] && continue
    if [ "${asset}" != "${gp7_last_asset}" ]; then
        mkdir -p "${OUT_DIR}/${asset}"
        gp7_last_asset="${asset}"
    fi
    out="${OUT_DIR}/${asset}/${file}"
    if [ -s "${out}" ]; then
        echo "[fetch-gltf-conformance] already have ${asset}/${file}, skipping"
        gp7_skipped=$((gp7_skipped + 1))
        continue
    fi
    mkdir -p "$(dirname "${out}")"
    encoded_asset="$(printf '%s' "${asset}" | sed 's/ /%20/g')"
    encoded_file="$(printf '%s' "${file}" | sed 's/ /%20/g')"
    url="${GP7_BASE_URL}/${encoded_asset}/glTF/${encoded_file}"
    echo "[fetch-gltf-conformance] fetching ${asset}/${file}"
    if ! curl -sfL -o "${out}.tmp" "${url}"; then
        echo "[fetch-gltf-conformance] ERROR: failed to fetch ${url}" >&2
        rm -f "${out}.tmp"
        exit 1
    fi
    mv "${out}.tmp" "${out}"
    gp7_fetched=$((gp7_fetched + 1))
done <<'GP7_TABLE'
AnimatedCube	AnimatedCube.bin
AnimatedCube	AnimatedCube.gltf
AnimatedCube	AnimatedCube_BaseColor.png
AnimatedTriangle	AnimatedTriangle.gltf
AnimatedTriangle	AnimatedTriangle_animation.bin
AnimatedTriangle	AnimatedTriangle_geometry.bin
BoomBoxWithAxes	BoomBoxWithAxes.bin
BoomBoxWithAxes	BoomBoxWithAxes.gltf
BoomBoxWithAxes	BoomBoxWithAxes_baseColor.png
BoomBoxWithAxes	BoomBoxWithAxes_baseColor1.png
BoomBoxWithAxes	BoomBoxWithAxes_emissive.png
BoomBoxWithAxes	BoomBoxWithAxes_normal.png
BoomBoxWithAxes	BoomBoxWithAxes_roughnessMetallic.png
Box With Spaces	Box With Spaces.bin
Box With Spaces	Box With Spaces.gltf
Box With Spaces	Normal Map.png
Box With Spaces	Roughness Metallic.png
Box With Spaces	glTF Logo With Spaces.png
Cameras	Cameras.bin
Cameras	Cameras.gltf
Cube	Cube.bin
Cube	Cube.gltf
Cube	Cube_BaseColor.png
EnvironmentTest	EnvironmentTest.gltf
EnvironmentTest	EnvironmentTest_binary.bin
EnvironmentTest	EnvironmentTest_images/roughness_metallic_0.png
EnvironmentTest	EnvironmentTest_images/roughness_metallic_1.png
FlightHelmet	FlightHelmet.bin
FlightHelmet	FlightHelmet.gltf
FlightHelmet	FlightHelmet_Materials_GlassPlasticMat_BaseColor.png
FlightHelmet	FlightHelmet_Materials_GlassPlasticMat_Normal.png
FlightHelmet	FlightHelmet_Materials_GlassPlasticMat_OcclusionRoughMetal.png
FlightHelmet	FlightHelmet_Materials_LeatherPartsMat_BaseColor.png
FlightHelmet	FlightHelmet_Materials_LeatherPartsMat_Normal.png
FlightHelmet	FlightHelmet_Materials_LeatherPartsMat_OcclusionRoughMetal.png
FlightHelmet	FlightHelmet_Materials_LensesMat_BaseColor.png
FlightHelmet	FlightHelmet_Materials_LensesMat_Normal.png
FlightHelmet	FlightHelmet_Materials_LensesMat_OcclusionRoughMetal.png
FlightHelmet	FlightHelmet_Materials_MetalPartsMat_BaseColor.png
FlightHelmet	FlightHelmet_Materials_MetalPartsMat_Normal.png
FlightHelmet	FlightHelmet_Materials_MetalPartsMat_OcclusionRoughMetal.png
FlightHelmet	FlightHelmet_Materials_RubberWoodMat_BaseColor.png
FlightHelmet	FlightHelmet_Materials_RubberWoodMat_Normal.png
FlightHelmet	FlightHelmet_Materials_RubberWoodMat_OcclusionRoughMetal.png
IridescenceDielectricSpheres	IridescenceDielectricSpheres.bin
IridescenceDielectricSpheres	IridescenceDielectricSpheres.gltf
IridescenceDielectricSpheres	textures/guides.png
IridescenceMetallicSpheres	IridescenceMetallicSpheres.bin
IridescenceMetallicSpheres	IridescenceMetallicSpheres.gltf
IridescenceMetallicSpheres	textures/guides.png
MandarinOrange	MandarinOrange.bin
MandarinOrange	MandarinOrange.gltf
MandarinOrange	MandarinOrange_Basecolor.jpg
MandarinOrange	MandarinOrange_DiffuseTransmission.png
MandarinOrange	MandarinOrange_Normal.png
MandarinOrange	MandarinOrange_OcclusionRough.jpg
MeshPrimitiveModes	MeshPrimitiveModes.gltf
MeshPrimitiveModes	buffer.bin
MeshoptCubeTest	MeshoptCubeTest.bin
MeshoptCubeTest	MeshoptCubeTest.gltf
MeshoptCubeTest	MeshoptCubeTestFallback.bin
MeshoptCubeTest	col0.png
MeshoptCubeTest	col1.png
MeshoptCubeTest	col2.png
MeshoptCubeTest	col3.png
MeshoptCubeTest	col4.png
MeshoptCubeTest	row0.png
MeshoptCubeTest	row1.png
MeshoptCubeTest	row2.png
MeshoptCubeTest	row3.png
MeshoptCubeTest	row4.png
MultipleScenes	MultipleScenes.gltf
MultipleScenes	MultipleScenes_square.bin
MultipleScenes	MultipleScenes_triangle.bin
PrimitiveModeNormalsTest	Colors.bin
PrimitiveModeNormalsTest	Labels.png
PrimitiveModeNormalsTest	Lines.bin
PrimitiveModeNormalsTest	Plane.bin
PrimitiveModeNormalsTest	Points.bin
PrimitiveModeNormalsTest	PrimitiveModeNormalsTest.gltf
PrimitiveModeNormalsTest	Triangles.bin
SciFiHelmet	SciFiHelmet.bin
SciFiHelmet	SciFiHelmet.gltf
SciFiHelmet	SciFiHelmet_AmbientOcclusion.png
SciFiHelmet	SciFiHelmet_BaseColor.png
SciFiHelmet	SciFiHelmet_MetallicRoughness.png
SciFiHelmet	SciFiHelmet_Normal.png
SheenCloth	SheenCloth.bin
SheenCloth	SheenCloth.gltf
SheenCloth	SheenCloth_AO.jpg
SheenCloth	technicalFabricSmall_basecolor_256.png
SheenCloth	technicalFabricSmall_normal_256.png
SheenCloth	technicalFabricSmall_orm_256.png
SheenCloth	technicalFabricSmall_sheen_256.png
SimpleMaterial	SimpleMaterial.bin
SimpleMaterial	SimpleMaterial.gltf
SimpleMeshes	SimpleMeshes.bin
SimpleMeshes	SimpleMeshes.gltf
SimpleMorph	SimpleMorph.gltf
SimpleMorph	SimpleMorph_animation.bin
SimpleMorph	SimpleMorph_geometry.bin
SimpleSkin	SimpleSkin.gltf
SimpleSkin	SimpleSkin_animation.bin
SimpleSkin	SimpleSkin_geometry.bin
SimpleSkin	SimpleSkin_inverseBindMatrices.bin
SimpleSkin	SimpleSkin_skinningData.bin
SimpleSparseAccessor	SimpleSparseAccessor.bin
SimpleSparseAccessor	SimpleSparseAccessor.gltf
SimpleTexture	SimpleTexture.bin
SimpleTexture	SimpleTexture.gltf
SimpleTexture	testTexture.png
Sponza	10381718147657362067.jpg
Sponza	10388182081421875623.jpg
Sponza	11474523244911310074.jpg
Sponza	11490520546946913238.jpg
Sponza	11872827283454512094.jpg
Sponza	11968150294050148237.jpg
Sponza	1219024358953944284.jpg
Sponza	12501374198249454378.jpg
Sponza	13196865903111448057.jpg
Sponza	13824894030729245199.jpg
Sponza	13982482287905699490.jpg
Sponza	14118779221266351425.jpg
Sponza	14170708867020035030.jpg
Sponza	14267839433702832875.jpg
Sponza	14650633544276105767.jpg
Sponza	15295713303328085182.jpg
Sponza	15722799267630235092.jpg
Sponza	16275776544635328252.png
Sponza	16299174074766089871.jpg
Sponza	16885566240357350108.jpg
Sponza	17556969131407844942.jpg
Sponza	17876391417123941155.jpg
Sponza	2051777328469649772.jpg
Sponza	2185409758123873465.jpg
Sponza	2299742237651021498.jpg
Sponza	2374361008830720677.jpg
Sponza	2411100444841994089.jpg
Sponza	2775690330959970771.jpg
Sponza	2969916736137545357.jpg
Sponza	332936164838540657.jpg
Sponza	3371964815757888145.jpg
Sponza	3455394979645218238.jpg
Sponza	3628158980083700836.jpg
Sponza	3827035219084910048.jpg
Sponza	4477655471536070370.jpg
Sponza	4601176305987539675.jpg
Sponza	466164707995436622.jpg
Sponza	4675343432951571524.jpg
Sponza	4871783166746854860.jpg
Sponza	4910669866631290573.jpg
Sponza	4975155472559461469.jpg
Sponza	5061699253647017043.png
Sponza	5792855332885324923.jpg
Sponza	5823059166183034438.jpg
Sponza	6047387724914829168.jpg
Sponza	6151467286084645207.jpg
Sponza	6593109234861095314.jpg
Sponza	6667038893015345571.jpg
Sponza	6772804448157695701.jpg
Sponza	7056944414013900257.jpg
Sponza	715093869573992647.jpg
Sponza	7268504077753552595.jpg
Sponza	7441062115984513793.jpg
Sponza	755318871556304029.jpg
Sponza	759203620573749278.jpg
Sponza	7645212358685992005.jpg
Sponza	7815564343179553343.jpg
Sponza	8006627369776289000.png
Sponza	8051790464816141987.jpg
Sponza	8114461559286000061.jpg
Sponza	8481240838833932244.jpg
Sponza	8503262930880235456.jpg
Sponza	8747919177698443163.jpg
Sponza	8750083169368950601.jpg
Sponza	8773302468495022225.jpg
Sponza	8783994986360286082.jpg
Sponza	9288698199695299068.jpg
Sponza	9916269861720640319.jpg
Sponza	Sponza.bin
Sponza	Sponza.gltf
Sponza	white.png
StainedGlassLamp	StainedGlassLamp.bin
StainedGlassLamp	StainedGlassLamp.gltf
StainedGlassLamp	StainedGlassLamp_base_basecolor.png
StainedGlassLamp	StainedGlassLamp_base_emissive.png
StainedGlassLamp	StainedGlassLamp_base_normal.png
StainedGlassLamp	StainedGlassLamp_base_occlusion-rough-metal.png
StainedGlassLamp	StainedGlassLamp_bulbs_occlusion.png
StainedGlassLamp	StainedGlassLamp_glass_basecolor-alpha.png
StainedGlassLamp	StainedGlassLamp_glass_emissive.png
StainedGlassLamp	StainedGlassLamp_glass_normal.png
StainedGlassLamp	StainedGlassLamp_glass_occlusion-rough-metal.png
StainedGlassLamp	StainedGlassLamp_glass_transmission-clearcoat.png
StainedGlassLamp	StainedGlassLamp_grill_basecolor-alpha.png
StainedGlassLamp	StainedGlassLamp_grill_emissive.png
StainedGlassLamp	StainedGlassLamp_grill_normal.png
StainedGlassLamp	StainedGlassLamp_grill_occlusion-rough-metal.png
StainedGlassLamp	StainedGlassLamp_hardware_basecolor.png
StainedGlassLamp	StainedGlassLamp_hardware_emissive.png
StainedGlassLamp	StainedGlassLamp_hardware_normal.png
StainedGlassLamp	StainedGlassLamp_hardware_occlusion-rough-metal.png
StainedGlassLamp	StainedGlassLamp_steel_occlusion.png
Suzanne	Suzanne.bin
Suzanne	Suzanne.gltf
Suzanne	Suzanne_BaseColor.png
Suzanne	Suzanne_MetallicRoughness.png
Triangle	Triangle.bin
Triangle	Triangle.gltf
TriangleWithoutIndices	TriangleWithoutIndices.bin
TriangleWithoutIndices	TriangleWithoutIndices.gltf
TwoSidedPlane	TwoSidedPlane.bin
TwoSidedPlane	TwoSidedPlane.gltf
TwoSidedPlane	TwoSidedPlane_BaseColor.png
TwoSidedPlane	TwoSidedPlane_MetallicRoughness.png
TwoSidedPlane	TwoSidedPlane_Normal.png
GP7_TABLE

# G-P6 — node.hdri_source's demo material. NOT part of the Khronos suite:
# Poly Haven CC0 4k equirect HDRI, direct URL, no commit-pin needed (single
# stable asset, not a versioned repo tree). Skip-demo-if-absent, same
# pattern as everything above and the held-out AMG fixture — G-P6's own
# tests generate a synthetic EXR in-process rather than depending on this
# file; only the acceptance-demo render (`render-import ... --param
# env_mode=1 --param hdri_file=...`) needs it. Never vendored/committed —
# see .gitignore.
HDRI_URL="https://dl.polyhaven.org/file/ph-assets/HDRIs/exr/4k/kloppenheim_07_puresky_4k.exr"
HDRI_OUT="${REPO_ROOT}/tests/fixtures/gltf/kloppenheim_07_puresky_4k.exr"
hdri_fetched=0
hdri_skipped=0
if [ -s "${HDRI_OUT}" ]; then
    echo "[fetch-gltf-conformance] already have kloppenheim_07_puresky_4k.exr, skipping"
    hdri_skipped=1
else
    echo "[fetch-gltf-conformance] fetching kloppenheim_07_puresky_4k.exr"
    if ! curl -sfL -o "${HDRI_OUT}.tmp" "${HDRI_URL}"; then
        echo "[fetch-gltf-conformance] ERROR: failed to fetch ${HDRI_URL}" >&2
        rm -f "${HDRI_OUT}.tmp"
        exit 1
    fi
    mv "${HDRI_OUT}.tmp" "${HDRI_OUT}"
    hdri_fetched=1
fi

echo "[fetch-gltf-conformance] done: $((fetched + tt_fetched + gp7_fetched + hdri_fetched)) fetched, $((skipped + tt_skipped + gp7_skipped + hdri_skipped)) already present"
