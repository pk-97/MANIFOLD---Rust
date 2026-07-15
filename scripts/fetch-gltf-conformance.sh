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

# Every asset named in manifest.json's glTF-Binary (.glb) variant, EXCEPT
# TextureTransformTest — see below.
ASSETS="
MetalRoughSpheres
EmissiveStrengthTest
ClearCoatTest
AlphaBlendModeTest
NormalTangentMirrorTest
SpecularTest
TextureSettingsTest
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

echo "[fetch-gltf-conformance] done: $((fetched + tt_fetched)) fetched, $((skipped + tt_skipped)) already present"
