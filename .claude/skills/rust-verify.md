# Verify Rust Parity with Unity

Compare a Rust implementation against its Unity source to identify divergences. This skill checks both structural and value-level parity.

User request: $ARGUMENTS

## Workflow

### 1. Identify the Files to Compare

From the user's request, determine:
- The Rust file(s) to verify
- The corresponding Unity .cs file(s)

### 2. Read Both Files

Read the Rust implementation AND the Unity source completely.

### 3. Structural Parity Check

Compare and report divergences in:
- [ ] Fields: all Unity fields present in Rust with correct types?
- [ ] Methods: all Unity methods present with same signatures?
- [ ] Interfaces/traits: same method surface?
- [ ] Base class patterns: preserved as trait + shared state?
- [ ] Service boundaries: same classes kept separate?
- [ ] Dependencies: same dependency direction?

### 4. Value-Level Parity Check (CRITICAL)

Compare and report divergences in:
- [ ] Constants: every value matches Unity exactly?
- [ ] Texture formats: `RFloat` → `R32Float` (NOT `Rgba16Float`)?
- [ ] Buffer sizes: match Unity's values (no invented platform limits)?
- [ ] Math operations: rounding, clamping, lerp behavior match?
- [ ] Parameter indices: match registry definitions?
- [ ] Shader uniforms: names and values match?
- [ ] Default values: match registry/constructor defaults?
- [ ] Pass count: same number of render/compute passes?
- [ ] Dispatch sizes: match Unity's compute dispatch?

### 5. Logic Flow Check

- [ ] Same branching structure (every `if` in Unity → `if` in Rust)?
- [ ] Same edge cases and guards preserved?
- [ ] Same early returns?
- [ ] Same order of operations?

### 6. Report

Output a categorized list:
- **Structural divergences** (architecture changes)
- **Value divergences** (constants, formats, math ops that differ)
- **Missing functionality** (methods/fields present in Unity but absent in Rust)
- **Added functionality** (things in Rust that Unity doesn't have)

For each divergence, state:
1. What Unity does (with file:line reference)
2. What Rust does differently
3. Whether this is a bug or intentional platform adaptation
