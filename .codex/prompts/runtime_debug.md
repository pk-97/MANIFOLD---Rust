# Runtime Debug Task Frame

Use this when the bug involves timing, ordering, callbacks, or mutable runtime state.

## Approach

1. Read the relevant Rust path briefly
2. Add targeted logs quickly
3. Reproduce
4. Read the logs
5. Fix the actual observed failure mode
6. Remove or reduce temporary logs before closeout unless they remain useful

## Logging Guidance

- Include enough context to reconstruct event order
- Prefer a few high-signal logs over noisy blanket logging
- Mirror Unity naming when logging translated concepts so parity checks stay easy
