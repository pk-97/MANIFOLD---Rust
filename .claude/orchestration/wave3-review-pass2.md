# Wave 3 — Review Pass 2 directives (Peter, 2026-07-22)

Peter's directive, verbatim: "I want this codebase to be optimal Rust and not have any of these god files or giant mess of bloated function calls etc. Simple, safe, easy to work with." The Wave 3 session folds ALL of this into a revised design (v2, PROPOSED) for Peter's approval before any execution.

## 1. File splits (supersedes "aggregate stays whole" file-monolithism — D-43)
Cohesion verdicts stand (no runtime/importer redesign), but every file gets the standard directory-module treatment, pure moves, move_identity-gated:
- `preset_runtime/` → {core (~2k: build/bind/rebuild), errors, segments (prewarm cluster), bindings (string/relight satellites)}
- `codegen/` → {types (param mapping tables), uniforms, kernel, entry_points}
- `gltf_import/` → BY FEATURE: {mesh, animation (skeleton/rigid/morph), materials, cards, report}
- Target: no file over ~1.5k of code.

## 2. Tests-out + house rule
Move the trailing test corpora (5.5k/4.1k/3.3k lines) to sibling `tests.rs` module files (same crate, private access, cfg(test) — the idiomatic large-corpus form). Propose as house rule for the design doc: in-file tests until ~500 lines, sibling file after.

## 3. Table-ization phase — the un-porting principle
Root cause named by Peter: the 1:1 Unity C# port turned class-per-thing/method-per-behavior into function-per-feature catalogs — facts wearing code's clothing. The revised design adds a DATA-DRIVEN phase where rows would outnumber emitter lines several-fold:
- PRIMARY TARGET: gltf_import's card emission (`card_param`/`card_binding` call clusters → typed const tables + one emitter walk). Expected: hundreds of lines deleted, adding a feature's params = adding rows.
- EVALUATE (design judgment, don't force): codegen's type-mapping helpers (already table-shaped — formalize), other row-heavy assembly sections found in the audit.
- CAUTIONS (binding): tables are typed Rust data (const tables / primitive!-style macro registries / serde-loaded JSON per the three proven in-repo precedents — 185 primitives, ParamRow projection, flow manifest), never stringly config; ONE consumer per table (a table two systems interpret differently is the parallel-dispatch disease — zero-new-systems applies to tables); table-ize only where rows ≫ emitter lines.

## 4. Bloated-call audit
While auditing, census wide parameter lists (the "giant mess of bloated function calls") in the three files; propose context-struct or table fixes per site where a signature exceeds ~6 params. Cross-ref: ROW_MODEL_EDGES on the register covers the ui-side instance (RowHost's ~11-param row_action).

## 5. Process
Revised design lands as RENDERER_RUNTIME v2 PROPOSED, adversarially reviewed (fresh Fable agent, same as pass 1), then Peter reviews. Execution stays fenced until his approval. Gates at execution: move_identity for all pure moves; for table-ization, value-level equivalence (the emitter's output vs the old per-feature functions' output on the canonical fixtures — the gpu_proofs value-parity model) + the held-out-input rule for the importer.
