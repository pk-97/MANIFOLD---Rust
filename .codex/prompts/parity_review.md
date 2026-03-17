# Parity Review Task Frame

Use this when a Rust file already exists and the task is to verify or audit parity.

## Compare

- Unity fields to Rust fields
- Unity methods to Rust methods
- interface/trait surface
- base-class responsibilities
- branching and guards
- constants, defaults, formats, math operations, param indices

## Report Format

- Structural divergences
- Value-level divergences
- Missing functionality
- Added functionality not present in Unity

For each finding, state what Unity does, what Rust does, and whether the difference is intentional or a bug.
