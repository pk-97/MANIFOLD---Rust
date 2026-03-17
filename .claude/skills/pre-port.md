# Pre-Port Dependency Analysis

Run this BEFORE porting any Unity file to Rust. Identifies dependencies, checks what's already ported, and determines the correct porting order.

User request: $ARGUMENTS

## Workflow

### 1. Identify the Target

What Unity file(s) are being ported? Read them to understand their dependencies.

### 2. Check PORT_STATUS.md

Read `docs/PORT_STATUS.md` to see if the target is already ported, partial, or missing.
- If `ported`: task is parity verification, not porting. Use `/rust-verify` instead.
- If `partial`: identify what's missing and port only the gaps.
- If `missing`: proceed with full port.

### 3. Dependency Analysis

Read the Unity source file and list ALL dependencies:

**Service dependencies:** Does this file call methods on other service classes?
- e.g., `EditingService`, `VideoLibrary`, `ProjectIOService`
- Are those services already ported as units in Rust? (Check PORT_STATUS.md)

**Infrastructure dependencies:** Does this file use shared infrastructure?
- e.g., `PlayerPrefs`, `DialogPathMemory`, `UserPrefs`, `FileDialogService`
- Is that infrastructure already ported? If not, it must be ported FIRST.

**Data model dependencies:** Does this file use data types from other files?
- These should all be in `manifold-core` already — verify they are.

**Interface dependencies:** Does this file implement or consume interfaces?
- Verify the corresponding Rust trait exists and has the same method surface.

### 4. Determine Porting Order

List files that must be ported BEFORE the target, in dependency order:
1. Infrastructure (UserPrefs, DialogPathMemory, etc.)
2. Services the target depends on
3. The target file itself
4. Callers/consumers that need updating

### 5. Check for Existing Inline Implementations

Search the Rust codebase for any inline implementations of the target's logic:
- Are there copy-pasted versions in `app.rs` or `ui_bridge.rs`?
- If so, the port must consolidate them into the service, not add another copy.

### 6. Report

Output:
- Target file and its Rust crate destination
- Dependencies (ported / missing)
- Recommended porting order
- Any existing inline code that should be consolidated
- Estimated scope (number of files to create/modify)
