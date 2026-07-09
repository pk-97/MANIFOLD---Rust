# Project File Integrity — a save format you can trust your show to

**Status:** APPROVED design, not built · 2026-07-09 · Opus (1M) · Peter in the room
**Prerequisites:** none (touches `manifold-core` version constant + `manifold-io` load/save; no design depends on this landing first)
**Execution contract:** read docs/DESIGN_DOC_STANDARD.md §5–§6 before starting any phase.

The instrument this protects is the `.manifold` file itself. Today three paths can silently
cost you work: **(BUG-062)** an older build opens a file written by a newer build, drops the
fields and effects it doesn't recognize, and writes the loss back on the next save;
**(BUG-064)** a power cut mid-save can leave a durably-renamed file pointing at unflushed,
torn bytes; **(BUG-065)** the 24-bit save-dedup hash can collide, making a real save look
identical to a prior one and get skipped. The governing insight: **the file must always be
either a faithful superset of what MANIFOLD read, or refused outright — never a silently
lesser version of it.** Because a build can't safely round-trip data it doesn't understand
(see D1), the only honest response to a newer file is to refuse it with a clear message, the
way Ableton does.

Peter's directives, load-bearing:
- On forward-compat: *"it should behave like Ableton and tell the user what version they need."*
- On why not read-only: *"it should refuse to open as read only won't make sense if features
  are missing etc"* — a half-rendered show is worse than a clean refusal.

Companion docs: [PROJECT_IO_MAP.md](PROJECT_IO_MAP.md) (current-state map of the load/save
pipeline this extends), [FOUNDATIONAL_GAPS.md](FOUNDATIONAL_GAPS.md) (the I/O data-loss
cluster this design closes). BUG-062/063/064/065 in [BUG_BACKLOG.md](BUG_BACKLOG.md).

---

## 1. Audit — what exists (verified 2026-07-09)

| Piece | Where | State |
|---|---|---|
| Forward migration chain | [migrate.rs:5-90](../crates/manifold-io/src/migrate.rs#L5-L90) `migrate_if_needed` | Migrates `projectVersion` *forward only* (`is_version_less_than(version, "1.x")` gates). A file whose version **exceeds** the build's max hits no gate, isn't migrated, and deserializes as-is. **No forward guard exists.** |
| Version comparison | [migrate.rs:724-742](../crates/manifold-io/src/migrate.rs#L724-L742) `is_version_less_than` | Private `fn`, semver-triple compare. Reusable for the guard once visibility widens. |
| Schema version literal | [project.rs:1508](../crates/manifold-core/src/project.rs#L1508) `"1.11.0"` **and** [migrate.rs:86](../crates/manifold-io/src/migrate.rs#L86) `"1.11.0"` | The current schema version, hard-coded in **two** places with no shared constant. |
| Unknown-field handling | model structs in `manifold-core` (`grep` for `deny_unknown_fields` → **zero hits** 2026-07-09) | serde **silently ignores** unknown fields on deserialize. Combined with the strip below, this is BUG-062's mechanism. |
| Unknown-effect strip | [loader.rs:179-181](../crates/manifold-io/src/loader.rs#L179-L181) `strip_unknown_effects()` | **Actively deletes** unrecognized effect types on load. Not latent — it runs every load. |
| Atomic save | [archive.rs:128-204](../crates/manifold-io/src/archive.rs#L128-L204) | Writes temp file → `zip.finish()` (closes, does **not** fsync) → `rename` → fsyncs the **parent directory**. The parent-dir fsync makes the rename durable; **nothing fsyncs the temp file's contents**, so the durable rename can point at unflushed bytes after power loss. |
| Dedup / history hash | [archive.rs:289-295](../crates/manifold-io/src/archive.rs#L289-L295) `compute_hash` | SHA-256 truncated to **first 3 bytes (24 bits)**. Used as `current_hash` (dedup: equal hash → skip save) and as history entry key `history/<hash>.json.gz`. |
| Load-error surfacing | [project_io.rs:408-417](../crates/manifold-app/src/project_io.rs#L408-L417) | `open_project_from_path`'s `Err` arm already routes `LoadError` into a native modal via `alerts::error("Couldn't Open Project", …{e}…)` — the G4 surface-don't-log pattern. **A new `LoadError` variant surfaces for free through its `Display`.** |
| Archive container version | [manifest.rs:10,32](../crates/manifold-io/src/manifest.rs#L10) `format_version: i32 = 2`; check at [archive.rs:259-263](../crates/manifold-io/src/archive.rs#L259-L263) | The ZIP-container format version (V1 plain-JSON vs V2 ZIP). A separate axis from `projectVersion`. No "max known container" ceiling exists. |
| Load call path | [project_io.rs:343-420](../crates/manifold-app/src/project_io.rs#L343-L420) → [loader.rs:37-79](../crates/manifold-io/src/loader.rs#L37-L79) `load_project_with` → [loader.rs:153-192](../crates/manifold-io/src/loader.rs#L153-L192) `load_project_from_json_with` → `migrate_if_needed` → `serde_json::from_str` | The one open path. The guard belongs at the **top** of `load_project_from_json_with`, before `migrate_if_needed`. |

*Extend, don't redesign.* Every change below slots into these existing seams. No new module,
thread, channel, or shared state.

## 2. Decisions

**D1 — Refuse a newer file; do not round-trip unknowns (v1).** When a file's schema version
exceeds this build's, refuse to open it with a message naming the version needed. *Rejected:
preserve-unknown-fields round-trip* (`#[serde(flatten)] extra: Map` catch-alls on every
serialized struct — `Project`, `Layer`, `Clip`, `PresetInstance`, the graph defs). Rejected
for v1 because it's a large, every-struct change, and even a perfect round-trip can't
*render* an effect the build doesn't have — so the show is still wrong on this machine.
Peter: *"read only won't make sense if features are missing."* Refusal is the honest,
bounded fix; round-trip is recorded in Deferred with its revival trigger. **Consequence,
stated honestly:** a file saved by a newer build is unopenable on the old build even if the
newer changes were purely additive — we treat every version bump as potentially lossy
because, without round-tripping, it is.

**D2 — `projectVersion` (the schema version) is what gates forward-compat, not the archive
`format_version`.** The schema version bumps on every field-shape change (the migration
chain is keyed to it); it is the tightest predictor of "this build might not understand this
file." The guard also does a coarse secondary check on `format_version` (D5) to catch a
future container-format bump, but the primary gate is `projectVersion`.

**D3 — One constant: `CURRENT_PROJECT_VERSION`.** The `"1.11.0"` literal at project.rs:1508
and migrate.rs:86 becomes a single `pub const CURRENT_PROJECT_VERSION: &str` in
`manifold-core`, read by (a) the `Project` default, (b) the migrate chain's final target,
(c) the forward guard's ceiling. *Rejected: leave the two literals and add a third for the
guard* — three copies of the schema version is exactly the drift class this file exists to
kill (mirrors the BUG_BACKLOG `**Status:` single-source fix). The migrate chain's
*intermediate* threshold literals (`"1.1.0"`…`"1.10.0"`) stay as-is: they are historical
constants, not the current version.

**D4 — The refuse message names the project-format version, not a marketing version.** No
schema-version → app-release-version map exists today. The message states the format number
honestly: *"This project was saved by a newer version of MANIFOLD (project format 1.12) than
this build can open (1.11). Update MANIFOLD to open it."* *Rejected: invent a
schema→"MANIFOLD 2.4" table now* — it would be fiction we'd have to maintain; recorded in
Deferred for when a real release-version scheme exists.

**D5 — The guard fires at two cheap sites, both producing `LoadError::TooNew`.** (1) JSON
level, at the top of `load_project_from_json_with`, before `migrate_if_needed`: parse
`projectVersion` out of the raw JSON (a `serde_json::Value` field read, the same cheap parse
migrate already does) and refuse if it exceeds `CURRENT_PROJECT_VERSION`. This is the
primary gate. (2) Archive level, in `load_project_with` after V2 detection: refuse if
`manifest.format_version` exceeds `CURRENT_ARCHIVE_FORMAT_VERSION` (a new `i32` const = 2).
This is the coarse secondary gate for a future container bump. *Why before migrate:* migrate
is forward-only and would silently pass a too-new file straight to deserialize (which drops
unknowns) — the guard must run first.

**D6 — Durability: fsync the temp file's contents before the rename.** Capture the `File`
that `zip.finish()` returns and `sync_all()` it before `std::fs::rename`. The existing
parent-directory fsync stays (it makes the rename itself durable). Together: contents on
disk, *then* a durable rename to them. *Rejected: write-then-rename without content fsync
(status quo)* — that ordering is the documented torn-file window (BUG-064).

**D7 — Widen the dedup/history hash to 64 bits (8 bytes).** `compute_hash` returns the first
8 bytes of the SHA-256 instead of 3. *Rejected: full 32-byte hash* — 64 bits already makes a
collision within one archive's bounded history (~50–200 live entries) negligible
(~10⁻¹⁵), and shorter keeps entry names and manifests compact. **Backward-compatible:** old
6-hex-char history entries keep their names and are copied forward untouched
([archive.rs:339-391](../crates/manifold-io/src/archive.rs#L339-L391) copies by name); the
manifest's `current_hash` simply transitions to the wider form on the next save; a 6-char
previous hash never equal-matches a 16-char new hash, so the worst case on the transition
save is one skipped dedup (a redundant save), never a wrong dedup.

## 3. Design body

### 3.1 The version constant (manifold-core)

```rust
// crates/manifold-core/src/project.rs  (or a new version.rs re-exported from lib.rs)
/// The schema version this build writes and is the newest it can open. Bumped
/// by every migration step that changes on-disk field shape; the migrate chain's
/// final target and the forward-compat guard both read it. Single source of truth.
pub const CURRENT_PROJECT_VERSION: &str = "1.11.0";
```

`Project::default()` sets `project_version: CURRENT_PROJECT_VERSION.to_string()`. The migrate
chain's final step (migrate.rs:84-87) targets `CURRENT_PROJECT_VERSION` instead of the
`"1.11.0"` literal.

### 3.2 The load error (manifold-io)

```rust
// crates/manifold-io/src/loader.rs — new LoadError variant
pub enum LoadError {
    Io(String),
    Migration(String),
    Deserialize(String),
    /// The file was written by a newer MANIFOLD than this build can open.
    /// `file_version` and `this_version` are the project-format versions (D4).
    TooNew { file_version: String, this_version: String },
}
```

`Display` for `TooNew` produces the D4 message verbatim (it flows unchanged into the existing
`alerts::error` modal at project_io.rs:413 — no app change). Example:

> This project was saved by a newer version of MANIFOLD (project format 1.12) than this build
> can open (1.11). Update MANIFOLD to open it.

### 3.3 The guard seam

`load_project_from_json_with` (loader.rs:153), **first statement**, before `migrate_if_needed`:

```
1. Parse `projectVersion` from the raw JSON as a serde_json::Value string field
   (absent → "1.0.0", matching migrate's own default; a legacy V1 file is never too new).
2. If is_version_less_than(CURRENT_PROJECT_VERSION, file_version)  → the file is newer →
   return Err(LoadError::TooNew { file_version, this_version: CURRENT_PROJECT_VERSION }).
3. Otherwise proceed to migrate_if_needed as today.
```

`load_project_with` (loader.rs:37), archive branch, after `extract_json_from_zip` succeeds:
read the manifest (`archive::read_manifest`) and if `format_version > CURRENT_ARCHIVE_FORMAT_VERSION`
return `Err(LoadError::TooNew { file_version: format!("archive v{format_version}"), this_version: format!("archive v{CURRENT_ARCHIVE_FORMAT_VERSION}") })`.

**Seam brief for `is_version_less_than`:** it is a private `fn` in migrate.rs. The guard needs
it. Change: `pub(crate) fn is_version_less_than` and call it from loader.rs as
`crate::migrate::is_version_less_than`. Do **not** duplicate the comparison logic (one
version-compare implementation in the crate). No call-site churn — widening visibility only.

### 3.4 Durability (manifold-io/archive.rs)

At [archive.rs:186](../crates/manifold-io/src/archive.rs#L186), replace the discard of
`zip.finish()` with a capture-and-sync:

```
old:  zip.finish().map_err(|e| format!("Failed to finish zip: {e}"))?;
new:  let file = zip.finish().map_err(|e| format!("Failed to finish zip: {e}"))?;
      file.sync_all().map_err(|e| format!("Failed to fsync temp file: {e}"))?;
```

`⚠ VERIFY-AT-IMPL:` confirm `ZipWriter::finish()` returns the inner `W` (the `File`) in the
pinned `zip` crate version — read the signature (`cargo doc`/source) before writing. If it
returns `()` there, capture the `File` handle before `ZipWriter::new` instead and sync that.
The parent-directory fsync at archive.rs:199-204 stays unchanged.

### 3.5 Hash width (manifold-io/archive.rs)

`compute_hash` returns 8 bytes of the digest instead of 3:

```
old:  format!("{:02x}{:02x}{:02x}", result[0], result[1], result[2])
new:  result[..8].iter().map(|b| format!("{b:02x}")).collect()   // 16 hex chars, 64 bits
```

No other change — callers treat the hash as an opaque string.

## 4. Phasing

Three phases, each one session, each committable and gate-passing on its own. P1 and P3 are
independent; P2 depends on P1's `CURRENT_PROJECT_VERSION` constant. Test tier per phase:
`manifold-io` focused (`cargo test -p manifold-io`) plus `-p manifold-core` where the const
moves; **no GPU** (nothing here touches a shader or the graph runtime). Full workspace sweep
runs once, in the final phase landed.

### P1 — Durability + single-source version constant + wider hash

**Entry state:** clean tree on `feat/project-file-integrity` at the design tip. Prove the
anchors: `rg -n 'first 3 bytes' crates/manifold-io/src/archive.rs` (hash site present),
`rg -n '"1.11.0"' crates/manifold-core/src/project.rs crates/manifold-io/src/migrate.rs`
(both literals present), `rg -n 'zip.finish\(\)' crates/manifold-io/src/archive.rs`.

**Read-back (first step, mandatory):** read §3.1, §3.4, §3.5 and D3/D6/D7; restate the
`CURRENT_PROJECT_VERSION` placement, the fsync capture, the hash width, and the forbidden
moves below. No code before this.

**Deliverables:**
- `pub const CURRENT_PROJECT_VERSION: &str = "1.11.0"` in `manifold-core`, referenced by
  `Project::default()` and migrate.rs's final chain step (both literals deleted).
- `compute_hash` → 8 bytes / 16 hex chars.
- `zip.finish()` captured and `sync_all()`'d before the rename.
- Tests: a `manifold-io` test that a save→reload round-trip still succeeds (existing
  `tests/load_project.rs` + `tests/history_snapshots.rs` cover this — run them); a test that
  two projects differing by one field produce different hashes (guards the widen didn't break
  hashing); the existing dedup/history tests stay green (no false dedup after widening).

**Gate:**
- Positive: `cargo test -p manifold-io -p manifold-core` green. Report the migrate chain's
  final-version test (`test_v100_chains_through_to_v140`) still asserts the current version.
- Negative (all must return **zero** hits):
  - `rg -n '"1.11.0"' crates/manifold-core/src/project.rs crates/manifold-io/src/migrate.rs`
    except the `const` definition and the intermediate-threshold lines — the *default* and the
    *final target* must now read the const. (Manually confirm the remaining hits are only
    intermediate thresholds `1.1.0`…`1.10.0` and the const itself.)
  - `rg -n 'result\[0\], result\[1\], result\[2\]' crates/manifold-io` — old 3-byte hash gone.
- **Durability is L0/L1 only** (fsync can't be unit-tested without fault injection): the gate
  is `rg -n 'sync_all' crates/manifold-io/src/archive.rs` returns the new temp-file call
  **and** the existing parent-dir call (two hits), plus no save/load regression. State this
  limit in the report; open a VERIFICATION_DEBT line for "power-loss durability unverified
  except by code inspection."

**Demo:** none — L1. No user-visible surface (durability + internal hash width). State
`Demo: none — L1` in the report.

**Forbidden moves:** widening the hash to full 32 bytes "while here" (D7 says 8 — scope
fence); "improving" the migrate chain's intermediate literals (leave them); touching the
parent-dir fsync; adding a third copy of the version string anywhere.

### P2 — Forward-version guard (BUG-062)

**Entry state:** P1 landed (const exists) — prove: `rg -n 'CURRENT_PROJECT_VERSION' crates/manifold-core/src`.
Prove the guard site is untouched: `rg -n 'migrate_if_needed' crates/manifold-io/src/loader.rs`.

**Read-back (first step):** read §3.2, §3.3, D1/D2/D4/D5; restate where the two guard sites
are, why the JSON guard runs before migrate, the exact `TooNew` message (D4), and that no app
change is needed.

**Deliverables:**
- `LoadError::TooNew { file_version, this_version }` + its `Display` (the D4 message).
- `is_version_less_than` → `pub(crate)`.
- `CURRENT_ARCHIVE_FORMAT_VERSION: i32 = 2` const in `manifold-io`.
- JSON-level guard at the top of `load_project_from_json_with` (§3.3 site 1).
- Archive-level guard in `load_project_with` V2 branch (§3.3 site 2).
- Tests in `manifold-io`:
  - **Round-trip gate (mandatory, §5 of the standard):** take a real fixture (or a
    `Project::default()` saved to a temp `.manifold`), rewrite its `projectVersion` to
    `"1.99.0"`, attempt `load_project` → assert `Err(LoadError::TooNew{..})` and that the
    message names `1.99` and `1.11`. Then rewrite to exactly `CURRENT_PROJECT_VERSION` →
    assert it loads. Then to `"1.5.0"` (older) → assert it loads (migrates forward).
  - **Held-out input:** a hand-written minimal JSON with `projectVersion: "2.0.0"` and an
    unknown top-level field → refused (proves the guard doesn't depend on fixture shape).
  - Archive guard: a manifest with `format_version: 3` → `TooNew`.

**Gate:**
- Positive: `cargo test -p manifold-io` green, including the three round-trip cases and the
  held-out input. Report the assertion that a current-version file still opens.
- Negative:
  - `rg -n 'fn is_version_less_than' crates/manifold-io/src` shows exactly **one** definition
    (no duplicated comparator).
  - No `unwrap()`/`expect()` on the `projectVersion` parse path (a malformed version string
    must degrade to "1.0.0"/not-too-new, never panic): `rg -n 'unwrap|expect' ` over the new
    guard lines returns zero.

**Demo (L2 — the acceptance artifact):** the refusal message text. Because the modal needs a
running app, the demo is a **test that prints the exact `Display` string** and a copy of that
string in the phase report, so a reviewer reads the real user-facing sentence. If a headless
harness can open the alert path, capture it; otherwise `Demo: L2 via Display string in
report` (the string is the user-visible surface here, and it is produced and read).

**Forbidden moves:** running the guard *after* migrate (must be before); duplicating the
version comparator instead of widening `is_version_less_than`; a silent fallback that opens a
too-new file read-only or partially (D1 — refuse, full stop); panicking on a malformed
version field; touching the app layer (the message surfaces through the existing modal).

### P3 — Surface silent load-repairs (BUG-063) · *Peter's call whether this lands in this wave*

Peter flagged 063 as "separate work." It is designed here so it's ready, and phased last so
the wave can land P1+P2 without it. **Blocking decision for entering P3:** Peter confirms it's
in this wave (else P3 is deferred with the trigger "when the load-repair-visibility work is
scheduled").

**Entry state:** P1+P2 landed. The repairs to surface are all in
[loader.rs run_post_load_validation](../crates/manifold-io/src/loader.rs#L197-L271):
`strip_unknown_effects`, `purge_orphaned_references`, `repair_overlapping_clips`,
envelope-orphan drops, `validate_clips` missing files. Each is currently `log::warn`/`info`
only.

**Read-back (first step):** read this phase + the §3-body note that no new mutation path is
added (this phase only *reports* what the existing repairs already do). Restate the forbidden
move: do not change what any repair does — only collect and surface it.

**Deliverables:**
- A `LoadReport` value (counts + short human lines: "3 unknown effects removed", "1
  overlapping clip repaired", "2 missing video files") accumulated during
  `run_post_load_validation` and returned alongside the `Project` (or hung on a field the app
  reads post-load).
- App surfacing at [project_io.rs open success arm](../crates/manifold-app/src/project_io.rs#L363-L407):
  if the report is non-empty, show it via the existing `alerts` / `toast` path — a
  *non-blocking* notice ("Opened with repairs: …"), distinct from the D1 hard-refuse modal.
- Test: a fixture (or synthesized project) with a known-unknown effect and an overlapping
  clip → load → assert the `LoadReport` names both.

**Gate:** `cargo test -p manifold-io` green; report the `LoadReport` contents for the test
fixture. **Demo (L2):** the notice text in the report; if `scripts/ui-flows` can drive the
open path, an L3 flow that opens a repairing fixture and asserts the toast.

**Forbidden moves:** changing any repair's behavior (delete-shorter-clip, strip-unknown stay
exactly as they are — this phase is visibility only); making the notice a blocking modal
(it's a non-blocking report, not an error); scope-creeping into "let the user undo the
repair" (Deferred).

## 5. Decided — do not reopen

1. Newer file → **refuse**, name the version needed. Not read-only, not best-effort. (D1, Peter.)
2. No unknown-field round-tripping in v1. (D1 — Deferred below with its trigger.)
3. `projectVersion` is the gate; `format_version` is a coarse secondary. (D2, D5.)
4. One `CURRENT_PROJECT_VERSION` constant; delete both literals. (D3.)
5. Message names the **project-format** version, not a marketing version. (D4.)
6. Guard runs **before** migrate. (D5.)
7. fsync the temp file's contents before rename; keep the parent-dir fsync. (D6.)
8. Hash → 64 bits, backward-compatible with 24-bit history entries. (D7.)
9. BUG-063 (surface repairs) is P3 and gated on Peter's go; it changes no repair behavior. (§4 P3.)

## 6. Deferred — not v1 (with revival trigger)

- **Unknown-field round-trip** (`#[serde(flatten)] extra` on every serialized struct, so an
  old build losslessly re-saves a newer file's unknown fields). *Revive when:* two shipping
  builds must interoperate on the same file with additive-only changes, i.e. when refusing
  becomes a real workflow cost on stage. Until then, refusal (D1) is correct.
- **Schema-version → marketing-version map** (so the message says "MANIFOLD 2.4" not "format
  1.12"). *Revive when:* a released-version scheme exists to map onto. (D4.)
- **Container-format (`format_version`) migration** beyond the coarse refuse — actually
  reading a future V3 container. *Revive when:* a V3 archive format is designed. (D5 handles
  refusal only.)
- **User-visible undo of a load-repair** (accept the repair, or open the pre-repair file
  read-only to rescue data). *Revive when:* a repair is found to drop data a user wanted.
  (P3 surfaces repairs; it doesn't offer to reverse them.)
