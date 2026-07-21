# Wave 2 sediment notes — for Peter (design D8: recorded, never fixed in-wave)

Result: **none found worth flagging.** All three execution seats (P2-G ×2, P2-E, P2-P) completed their moves without encountering dead shapes or duplicated concerns that survived a `git log -S` check. Two pre-answered items from the design audit, restated so nobody "cleans them up" later:

- `EffectContainer` (now `effects/mod.rs`) looks vestigial and is NOT — implemented by `Layer` (layer.rs:956) and `ProjectSettings` (settings.rs:433).
- The two `RenameGroupCommand` types (commands/effect_groups.rs vs commands/graph/groups.rs) are genuinely distinct commands, not accidental duplication — the name collision is guarded by the multiset census (design D7, review H1).
