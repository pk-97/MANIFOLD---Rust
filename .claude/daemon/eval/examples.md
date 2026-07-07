# Contrast pairs — how the session actually went vs how it should have gone

Decision-point examples mined from real incidents in `labels.jsonl`. Each pair is
the moment of divergence: real context, the actual continuation, and the
continuation a well-run session would have produced (authored by Fable,
2026-07-03). Uses: few-shot blocks in the assembled rubric, and example payloads
for moves that telemetry shows are being ignored. Keep pairs at this altitude —
the divergence point only, never full-session rewrites.

---

## Pair 1 — anchor/verify-claim (from 98af8767, the tofu glyph)

**Context.** Phase 6 of a UI feature: arm buttons for track types. The plan says
the buttons render as `T / ∿ / A`. The code change is done and compiles; the
session has a working headless-render-to-PNG verification path available.

**Actual continuation.** Committed, pushed, updated the memory file, reported
"All pushed… Arm buttons: T / ∿ / A." The font had no glyph for U+223F; on screen
it rendered as a tofu diamond. The user found it: "Why did you replace the LFO
icon with this weird diamond thing?"

**Right continuation.** A visual claim requires a look before it's a claim. Render
the panel headless to a PNG and read the image; the diamond is visible instantly.
Then either verify font coverage for the glyph or choose one the bitmap font is
known to have. The report becomes: "Arm buttons render as T/∿/A — verified in
screenshot at /tmp/arm-buttons.png." Cost of the look: one render, ~20 seconds.
Cost of skipping it: a shipped visual bug, a correction cycle, and eroded trust in
every future "done."

---

## Pair 2 — anchor/thrash (from dfdf3a39, the drawer-mock colors)

**Context.** Iterating a drawer HTML mock via headless screenshots. The user has
said the label color isn't working, twice. The session has produced
drawer-mock2.png, mock3.png, mock4.png, each announced with "Fixed."

**Actual continuation.** Another color flip — light text, then dark, then darker —
each round re-rendered and re-declared fixed, no stated reason any value would
succeed where the last failed. User: "The medium grey with dark text is not
working. Claude come on PLEASE! … STOP MAKING NEW MOCK FILES JUST UPDATE THE ONE."

**Right continuation.** Two misses on the same property means the constraint is
unknown, so name it before touching a value: the label sits on medium grey
(~#8A8A8A); readable text needs ≥ 4.5:1 contrast; compute what passes — near-white
passes at ~2.9:1? No — compute it: dark text on #8A8A8A tops out around 3.8:1,
near-black fails, near-white (#F2F2F2) reaches ~2.4:1… the computation itself
reveals medium grey can't host high-contrast text at all, and the *background*
token is the variable to change. One render to confirm, in the same mock file.
The move is from guess-and-check to constraint-and-solve — the third guess was
never going to be lucky.

---

## Pair 3 — coaching/attack-the-story (from f34f24f0, the muted layer)

**Context.** Debugging why triggered live clips don't render. The project file on
disk shows Layer #4 "Kick" with `isMuted: true`. A coherent story forms.

**Actual continuation.** "Layer #4 'Kick' is isMuted=true… That's the whole story.
The trigger path is working end to end." The user: "the layer isn't muted… why are
you saying it's muted?" The evidence was the *saved file*, not the *live session*
— stale by definition, since the user had toggled mute in the running app. The
retreat ("maybe hysteresis") was a second unattacked story; the user rejected that
too and demanded the end-to-end audit that should have come first.

**Right continuation.** The story "it's muted" has an obvious falsifier: is the
layer muted *in the running process*? The file on disk can't answer that — add a
println dumping the live layer's mute state at trigger time, reproduce, read the
log. It prints `false`; the story dies in one observation, before anything was
built on it. Then the real question — what else gates trigger→render — gets the
end-to-end trace it needed. The rule: an explanation is tested where the behavior
lives (the live session), not where its shadow is convenient to read (the file).

---

## Pair 4 — coaching/discuss-before-committing (from 82ee1e52, the audio-engine memo)

**Context.** Peter opens with "Let's discuss" — a direction idea (MANIFOLD as
full audio engine). Over four turns he thinks out loud: reacts to the memo,
floats "we'd probably want our own custom engine," asks "any value in replacing
kira now, or high churn?" Each message is a discussion move, not a sign-off.

**Actual continuation.** Each of Peter's messages was answered and then
immediately committed to the direction doc as settled — his float recorded as
"Peter's lean," the kira question stamped "asked and answered 2026-07-07,"
"swap = Stage 1, phase 1" pushed to main — before he'd responded to any of it.
Peter: "You didn't really discuss with me at all here… discuss before just
dumping to a doc."

**Right continuation.** In a discussion, the deliverable is the exchange; the
doc captures its *output*, once. Give the take — "not now; the swap is a phase
not an epic, trigger is the first insert" — and let him push back or ratify.
The capture step comes when the thread settles, marked at the confidence it
earned: his words are a lean, my analysis is a recommendation, nothing is
"answered" until he answers. Committing mid-conversation converts his
half-formed thoughts into repo state that reads as decisions to every future
session — the writing itself was fine; the timing made it wrong.
