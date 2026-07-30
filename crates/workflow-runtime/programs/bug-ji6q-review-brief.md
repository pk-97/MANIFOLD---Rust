# Review: does this test pin the described mechanism?

A regression test was just written for BUG-ji6q and proved red. Your job is to say whether
it fails for the RIGHT reason.

The mechanism, as diagnosed: card-stamped defs give every node param a `BindingDef` whose
`default_value` is a frozen snapshot of `node.params` at stamp time. On every graph build
`instantiate_def` writes the def's params onto the live node, then `apply_binding_defaults`
replants every binding's `default_value` over the top. Any writer touching `node.params`
for a bound target is silently reverted at the next rebuild. The per-frame `ParamManifest`
path (`bound.apply`) is immune — live macro sliders are unaffected.

What the lane did:

{{write-failing-test}}

Judge only these, by reading:

1. Does the test mutate a bound param AFTER the stamp and assert survival across a REBUILD?
   A test that only checks a value at build time proves nothing about this bug.
2. Is it exercising a path the bug actually bites — a def edit, a migration, or a direct
   `node.params` write — rather than the immune per-frame manifest path?
3. Would the fix described in the bead (seed the binding skip-cache from the live graph
   value instead of replanting `default_value`) turn this test green? If it would still
   fail after that fix, say so and say why.
4. Does the failure message name expected versus found, so the test reads as a contract?

Reject if any of 1-3 fails. A weak failure message alone is a note, not a rejection.

Answer as JSON matching:

    {"verdict": "accept" | "reject", "rationale": "<at least 20 characters>"}

The rationale names the specific thing you checked, not a summary of the brief.
