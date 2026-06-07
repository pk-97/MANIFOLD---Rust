# Preset Fork Collapse — Execution Contract (the goal)

**Mandate.** Collapse every effect/generator fork in
[`PRESET_FORK_INVENTORY.md`](PRESET_FORK_INVENTORY.md). All 13 structural forks
(#1–#13) + 3 capability gaps (#14–#16) + the Phase-4 UI. No deferrals. If it is on
the list it gets implemented, tested, and reviewed. Nothing is "left for later",
nothing is "out of scope", nothing is patched with a second fork. This runs
automated — no permission prompts, no "want me to proceed?". Commit + push each
batch when green (durable permission already given).

**Definition of DONE is behavioral, grep-proven — not "it compiles":**
for every action a preset can take (set param, reset-to-default, drive, envelope,
trim, Ableton-map, snapshot, persist, skip, string-bind), a generator and an effect
execute the *same* code path, and grep shows **zero** of these surviving:
`GraphParamTarget::Generator`, `preset_definition_registry::generator` (as a fork of
`::effect`), `resolve_gen_param_slot`, `evaluate_gen_param_envelopes`,
`gen_params_to_config`, `active_generator_graph_snapshot`, `generator_graph_version`,
`generator_type_registry` (as a fork of `effect_type_registry`),
`bundled_generator_presets` (as a fork of `bundled_presets`), the duplicate
`set_param_base`/`get_param_base` accessors, the `base_param_values: Option<Vec<f32>>`
residue, and the `GenParam` Ableton dispatch arm being handled differently from
`MasterEffect`/`LayerEffect`. Generators gain skip-mode; effects gain string-bindings.

## Batches (migrate several forks, THEN test — per Peter's rule)

**A — Registry keystone (#1 #2 #3 #4).** One definition registry, one type/picker
registry, one bundled loader, `LoadedPresetView` resolves for generators too. This is
the root: every downstream fork only forks because it asks "which registry?".

**B — Param resolution + accessors + residue (#5 #6 #16 #13).** One kind-correct
param-id→slot resolver (this *is* the right-click reset snap-back fix — fixed at the
source via the unified view, not mirrored), one accessor name, `base_param_values`
folded into the unified `Vec<ParamSlot>`, one version accessor.

**C — Modulation walk (#7).** One envelope/driver evaluation walk over
`PresetInstance`s regardless of kind; shared apply core already exists.

**D — UI/app dispatch (#8 #9 #10).** Collapse `GraphParamTarget` to one path (delete
the `Generator` arm + 23 paired inspector actions), one state-sync config builder, one
editor snapshot entry.

**E — Core dispatch + persistence (#11 #12).** One Ableton dispatch path, one
persistence migration.

**F — Capability gaps (#14 #15).** Generators declare skip-mode; effects expose
string params. Both become one-line enables once A–D land.

**G — Phase-4 UI.** Picker lists embedded presets; make-unique (`ForkPresetCommand`)
wired; export/import menu (`rfd` + `manifold_io::preset_file`).

## Per-batch gates (all headless, all automatable — no prompts)

1. `cargo clippy --workspace -- -D warnings` (cheap, every batch).
2. Focused tests for the crates the batch touched:
   `cargo test -p manifold-core --lib`, `-p manifold-editing --lib`,
   `-p manifold-io --lib` (golden Liveschool round-trip lives here),
   `-p manifold-playback --lib`, and the relevant `-p manifold-renderer --lib`
   targets (`preset_runtime`, `bundled_presets`).
3. `cargo run -p manifold-renderer --bin check-presets`.
4. **Grep-audit**: the fork symbols this batch killed return zero results.
5. Commit + push.

## Final acceptance (after G)

- Full `cargo test --workspace` sweep green, minus the known pre-existing fails only:
  `lut1d` + `watercolor` parity (fusion-vs-legacy divergence) and the documented
  Liveschool `FluidSimulation` Ableton-mapping test. Any *new* failure is mine to fix.
- `cargo clippy --workspace -- -D warnings` clean.
- Adversarial multi-agent review (correctness / completeness-grep / regression-risk /
  hot-path) over the full diff; every confirmed finding fixed before close.
- Mark each fork DONE in `PRESET_FORK_INVENTORY.md` with the commit that killed it.

## Anti-laziness rules (self-binding)

- "Done" never means "compiles" or "headless tests pass" — it means the grep is clean
  and the behavior is one path. I proved this wrong eight times by stopping at green.
- Never fix a fork by adding a kind-branch (mirror). The fix removes the branch.
- No `cd` prefix on Bash. No echo/tail/head/cat in non-pre-approved compounds.
- Don't ask permission between batches; the authorization is standing.
