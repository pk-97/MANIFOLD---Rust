# Wave 3 sediment notes — for future waves (design D8 policy by reference)

Same policy as `docs/notes/wave2-sediment-notes.md`: things noticed during the wave, recorded so a future wave doesn't rediscover them or "clean them up" wrongly — never fixed in-wave.

## Process lessons worth carrying forward

- **residue-0 ≠ compiles (W3-D5).** The crashed P3-G seat reported "extraction complete" at residue 0, but the tree did not build — 73 wiring gaps (missing `use` lines, `ObjectGroupOutput` field widenings). The move gate is blind to *absent* wiring. Binding rule for all future move phases: a phase is not done until `cargo check` (including the `gpu-proofs` config) is green. Seat-brief templates carry this line now.

- **The frozen move-verifier grew three new wiring classes this wave (W3-D2..D4), all in one shared commit V (`1cdc291c`), merged (never cherry-picked) into every lane so the lanes stayed disjoint and D-19's second-lander rule was satisfied by construction:**
  - *1-to-1 test-mod conversion class* (W3-D2): converting an inline `#[cfg] mod X { … }` to a `mod X;` declaration re-adds the cfg line 1-to-1; git self-pairs it and the D7a state machine never armed → false residue on the `-mod X {` line. The class allows only the mod-header/cfg wiring lines; any body byte alongside is still residue.
  - *context-cfg variant* of that class (W3-D4): git keeps the `#[cfg]` line as *context* (not a signed line) for `#[path]` declarations; arm on the context cfg opener (the D-20(i) analog).
  - *use-block opener class* (W3-D3): arm `open_block` on a signed `use …::{` line BEFORE the is-moved short-circuit, so reshuffled multi-line use lists whose identical openers git self-pairs as moved don't strand their item lines as residue. USE_ITEM smuggle-proofing unchanged.

- **The `#[path]` tests pattern (W3-D1).** D3's literal "tests/mod.rs declaring the mods" would have renamed every test's module path (`<mod>_tests::` → `tests::<mod>::`), failing the gpu-test LIST name-diff gate and forcing super-depth body rewrites the frozen verifier counts as residue. Ratified form: `#[cfg(...)] #[path = "tests/<file>.rs"] mod <original>_tests;` in the parent `mod.rs` — files live in `tests/`, module identity and bodies stay byte-identical, `include_str!` depth covered by the S0 prefix class. Use this shape whenever a large test corpus splits into multiple sibling files.

- **Gate economy held (D-45).** The WGSL byte gate superseded gpu-proofs execution for byte-gated changes; gpu-proofs ran exactly once (P3-Z); move phases used a gpu-test LIST census (`--features gpu-proofs -- --list`, builds only, name-diff empty); landings batched into three full sweeps, not six. No GPU-behavior regression surfaced at the single P3-Z run.

## Code observations (recorded, not fixed)

- **Preserved oddities in the base-color texture block (P3-D T1, INV-R8).** It is deliberately NOT a `MAP_FAMILIES` row: it alone increments `textures_wired`, and it pre-dates `map_tex_cache` so it does not participate in texture reuse. Both behaviors are preserved on purpose; the INV-R8 def-JSON diff exists to catch any future drift. Do not "unify" it into the table.
- **T3 (codegen param-row unification) is rejected on a corrected audit — do not re-propose.** The `param_wgsl_type` (Errs on Vec3/Vec4/Color) vs `param_word_count` (returns 3/4) asymmetry is a documented invariant, not drift; `param_is_fusable` is a one-line delegation. A unifying `ParamRow` would be more machinery to erase a fact.
