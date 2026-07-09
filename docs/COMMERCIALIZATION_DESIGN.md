# Commerce Infrastructure — License, Trial Watermark, Updater, Crash Telemetry

**Status: APPROVED design, not built · 2026-07-03 · Fable**
**Scope: the code half of the business layer. The business decisions themselves
(positioning, pricing, cohort, marketing) live in `BUSINESS_PLAN.md` — written
for Peter, not agents; read it for the why, this doc for the how.**
**Prerequisites: none hard; P4 telemetry rides GIG_RESILIENCE P1–P2 breadcrumbs.
All four phases ship inside v1.0 (launch gate — DESIGN_BUILD_ORDER §3).**
**Execution contract: read `docs/DESIGN_DOC_STANDARD.md` §5–§6 and §8 before
starting any phase. Hardening level: conformance (§9) — re-derive all anchors
at implementation; this doc predates execution by months.**

---

## 1. Shape

No new crate: a `commerce` module in `manifold-app` (license verification,
update checks, telemetry consent + upload). The trial watermark is the one
renderer-adjacent piece — it composites in the present/export paths, outside
the graph.

Decisions inherited from BUSINESS_PLAN.md (do not reopen here):

- **Licensing is offline-first and can never block a show.** Signed license
  file, verified locally; zero runtime phone-home. Activation may touch the
  network once; running never does.
- **Trial = watermark, forever.** Fully functional, watermark on video output
  until licensed (Resolume model). No time limit, no feature gating, no online
  DRM.
- **Updates never touch a show.** Channels (stable/beta); perform mode
  suppresses even the update check.
- **Telemetry is opt-in, upload only in edit mode.**

`⚠ VERIFY-AT-IMPL`: re-derive the present/export call sites before P2:
`rg -n 'fn present|fn export_frame' crates/manifold-app/ crates/manifold-media/`.

## 2. Phases

Forbidden, all phases: online DRM or runtime license checks · feature-gated
trial · updater actions inside perform mode · telemetry without opt-in ·
watermark as a graph node or project-level flag.

- **P1 — License file + verification.** Ed25519-signed license file (name,
  email, seat kind, update-pass expiry); local verification at startup; UI
  surface = about/registration panel. Unlicensed ≠ degraded: everything runs,
  P2 watermark applies. Expired update pass = still licensed for versions
  released before expiry. Gate: unit tests — valid/tampered/expired-pass
  licenses verify correctly; negative gate — `rg -n 'reqwest|ureq|curl'` in the
  verify module hits nothing (no network on the run path).
- **P2 — Trial watermark.** Composited in the **present + export paths**, after
  the graph, before output — structurally unbypassable from the graph editor or
  project data. Subtle but unmistakable (corner mark + periodic sweep, Resolume
  precedent). Gate: PNG test — watermarked output differs from licensed output
  only in the mark region; a graph-side bypass attempt (any project content)
  cannot remove it.
- **P3 — Auto-update.** Sparkle (macOS standard) with stable/beta channels and
  the show-freeze stance: never interrupts, never auto-installs; perform mode
  suppresses the check itself. Gate: update check provably runs only in edit
  mode; signed appcast verified.
- **P4 — Crash-telemetry upload.** Opt-in consent UI; uploads the existing
  GIG_RESILIENCE breadcrumb/crash.log bundle on next edit-mode launch after a
  crash. Never at show time, never without consent. Gate: consent-off = zero
  network (negative gate as P1); uploaded bundle content matches the on-disk
  breadcrumb.

## 3. Deferred (with triggers)

- **Tutorials + user manual** — a 100% need for launch (Peter, 2026-07-09), not
  optional polish. Deliberately not started yet; the material would rot while
  the surface is still moving. Copy register per the product-copy-voice memory
  (Ableton/TD grade). Trigger: v1.0 feature freeze.
- **Seat management / license reissue tooling** — manual (email) until volume
  hurts; then a small merchant-of-record webhook → license-file generator.
- **Windows updater** (Sparkle is macOS-only; WinSparkle or equivalent) — with
  the v1.5 Vulkan/Windows release, not before.
- **Venue/team multi-seat licenses** — when a venue actually asks.
