# Audit Parity — Batch Post-Port Verification

Run this AFTER a porting session to catch value drift, missing edge cases, and structural divergence across all recently changed files.

User request: $ARGUMENTS

## Workflow

### 1. Identify Changed Files

If the user specifies files, use those. Otherwise, check recent git changes:
```
git diff --name-only HEAD~1  (or appropriate range)
```

Filter to `.rs` and `.wgsl` files in the renderer, playback, editing, core, and ui crates.

### 2. For Each Changed Rust File

Determine the corresponding Unity source file using `docs/PORT_STATUS.md` and the crate mapping in `CLAUDE.md`.

### 3. Run Parity Checks

For each Rust↔Unity file pair, check:

**Structural parity:**
- [ ] All Unity fields present in Rust struct
- [ ] All Unity methods present in Rust impl
- [ ] All interface methods present in Rust trait impl
- [ ] Base class pattern preserved (trait + shared state)
- [ ] Service boundaries preserved (not scattered inline)

**Value parity (most bugs hide here):**
- [ ] Every constant matches Unity's exact value
- [ ] Every texture format matches (R32Float for RFloat, NOT Rgba16Float)
- [ ] Every math op matches (round vs truncate, clamped lerp vs unclamped)
- [ ] Every parameter index matches the registry definition
- [ ] Every default value matches
- [ ] Every buffer size / dispatch size matches

**Logic parity:**
- [ ] Same number of passes (multi-pass not collapsed to single-pass)
- [ ] Same branching structure (every if/guard/early-return preserved)
- [ ] Same edge cases (micro-clip skip, pending pause, recently started)
- [ ] Texel sizes from source texture, not target

**Shader parity (for .wgsl files):**
- [ ] Same math as HLSL, same variable names, same coordinate space
- [ ] Same texture sampling modes
- [ ] Same uniform/param mapping as SetUniforms()

### 4. Check KNOWN_DIVERGENCES.md

For any differences found: is it listed in `docs/KNOWN_DIVERGENCES.md`?
- If yes: it's approved, skip it.
- If no: it's a bug. Report it.

### 5. Report

Output a categorized list:

**BUGS (divergences not in KNOWN_DIVERGENCES.md):**
- File, line, what Unity does, what Rust does differently

**WARNINGS (suspicious but may be intentional):**
- Items that look different but might have a reason

**CLEAN:**
- Files that pass all checks

### 6. Update PORT_STATUS.md

If any file status changed (e.g., `partial` → `ported`), update `docs/PORT_STATUS.md`.
