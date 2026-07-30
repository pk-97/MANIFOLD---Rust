You are a code reviewer. Below are three ChangeSets that were applied and gate-green
(clippy + full rt_ gpu-proofs + a byte-identity render diff for the first one), plus the
measured bleed-probe result. Rule ONLY on conformance to the written invariants — the
gates already ruled on compile/test health.

Invariants (RAYTRACING_DESIGN.md section 11):
- I-MB3: the sun-bounce caster loop has exactly one home (`sun_bounce_at_hit`); both the
  GI gather and the reflection block call it; the historical rand2 seed streams
  (`400u + s * MAX_RT_CASTERS + sc`, `500u + sc`) are reproduced exactly.
- MB5: the gather's primary surface stays demodulated — throughput starts at 1.0 and is
  multiplied by INTERMEDIATE hit albedo only, times RT_GI_THROUGHPUT_FOLD, only on path
  extension.
- MB3: no environment/ambient term is added by the gather at ANY depth — a GI ray miss
  contributes nothing at every bounce.
- MB4: RT_GI_MAX_BOUNCES is 2 after MB-B, a named MSL constant, no runtime knob added.
- Scope: no edits outside the GI gather, the helper, the reflection block's
  sun_bounce_term call, and the MB-C test file + its mod registration.
- Probe: control_pass and bleed_pass are both true in the probe result.

Reject if any invariant is violated or any edit strays outside the stated scope. The
rationale must name the specific invariant and evidence (at least 20 characters).

Respond with JSON only: {"verdict": "accept" | "reject", "rationale": "<1-3 sentences>"}

# ChangeSet MB-A

{{refactor-bounce-loop}}

# ChangeSet MB-B

{{add-second-bounce}}

# ChangeSet MB-C

{{pin-regression-test}}

# Probe result

{{rule-on-bleed-probe}}
