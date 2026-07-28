---
name: lsp
description: Use the LSP tool (rust-analyzer) for Rust symbol questions instead of grep — where is this defined, what calls it, what implements this trait, what's in this file. Semantic, type-aware, follows trait dispatch and re-exports across crates. Invoke when you catch yourself about to grep for a Rust symbol, or when a symbol question needs a precise answer text search can't give.
---

# LSP — symbol intelligence for Rust

The native `LSP` tool is backed by rust-analyzer and works on this workspace.
Reach for it on **symbol-level questions**. It answers with semantics, so it has
none of grep's false positives (comments, strings, a same-named method on a
different type, re-exports).

This exists because Claude Code's system prompt biases toward grep, and a passive
"prefer LSP" line loses to it (~1% real-world LSP use). This skill + the
`lsp-nudge` hook are the affordance that wins.

## Pick the operation

| Question | Operation |
|---|---|
| Where is this symbol defined? | `goToDefinition` (on a use site) or `workspaceSymbol` (by name) |
| What calls this function? | `prepareCallHierarchy` then `incomingCalls` |
| What does this function call? | `outgoingCalls` |
| What implements this trait? | `goToImplementation` (on the trait name) |
| What's the type / docs here? | `hover` |
| What's in this file? | `documentSymbol` (one file, no index needed) |
| Find a symbol by name anywhere | `workspaceSymbol` (needs the warm index) |

All ops take `filePath`, `line`, `character` (1-based, as shown in the editor).
`workspaceSymbol` also needs `query` — never pass it empty.

## Gotchas

- **Index warm-up:** single-file ops (`documentSymbol`, `hover`, `goToDefinition`)
  work immediately. `workspaceSymbol` / `goToImplementation` need the full index —
  empty results right after a cold start mean *still indexing*, not *no results*.
  Retry after ~a minute.
- **Find the position first:** to act on a symbol you don't have a line/char for,
  start with `workspaceSymbol` (by name) or `documentSymbol` (within a file), then
  use the returned location for `goToImplementation` / `incomingCalls`.
- **Not for non-code:** LSP can't see preset JSON, log strings, config, or docs.
  Use `rg` for those — that's a legitimate text search, not a symbol question.

## When grep is still right

String/JSON/log/comment searches, any non-`.rs` file, or a fast textual sweep.
If the `lsp-nudge` hook blocks a grep you genuinely meant as text, re-run it with
`#grep-ok` appended.
