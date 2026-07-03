# MANIFOLD Business Plan

**Status: agreed direction · 2026-07-03**
**Audience: Peter. This doc is the playbook you execute in the real world — plain
language on purpose. The one piece agents build (licensing, trial watermark,
updater, crash telemetry) lives in `COMMERCIALIZATION_DESIGN.md`; release
sequencing lives in `DESIGN_BUILD_ORDER.md`.**

---

## 1. What you're selling, and to whom

MANIFOLD is for the step between independent musician and touring professional
team: artists who want a $1M-looking show without the $1M production budget. The
pro touring teams will keep using the industry standards — that's fine, they're
not your market. The market is you, five years ago.

The pitch rests on four pillars, not one killer feature:

1. **Whole stage from one laptop** — video, LED, projection, lighting triggers.
2. **Survives the gig without a crew** — crash recovery, understudy, autosave.
3. **Speaks your DAW natively** — drop your Ableton set, get a show.
4. **Agents as crew** — the AI does what a hired operator would.

Beat-native sync is real but it's one pillar, not the brand — most competitors
can do some beat sync, and its value depends on how close the performer is to the
music. Don't over-hinge on it.

The crash story is always the demo, never a promise: film the kill-9-mid-show
recovery where the audience never notices. Never claim "it doesn't crash" —
that claim invites the one counterexample.

## 2. The releases and what they mean for money

- **v1.0 — everything core.** Waves 0–3 of the build order plus the commerce
  infra: the full instrument, automation lanes, ML nodes, component library, MCP
  agent. Feature-complete out of the box — buyers aren't waiting on hype reels
  for basic functionality. This is when revenue starts.
- **v1.x — free updates.** Small flexes (extra ML tasks, preset packs) plus the
  two import funnels: **Resolume import** and **TouchDesigner import**. These
  only need v1.0 pieces, and each one is a switcher campaign, not just a feature.
- **v1.5 — Windows.** The Vulkan port. Quiet, long work — the v1.x campaigns
  above are what keep the feed alive during it.
- **v2.0 — the 3D track, as a paid upgrade.** Materials, real-time 3D scenes,
  cloth and water simulations, Blender/glTF import. This is the jaw-dropper
  release existing users pay to upgrade to — which is why it's held out of v1.0.

## 3. Price

**Perpetual license + 12 months of updates. No subscription.** Your audience
distrusts subscriptions (see the resentment around Notch); pay-once is the
Ableton/Resolume pattern and it matches "value their payment."

Recommended: **list $299 USD, launch price $249**. Your instinct ($250) is right
as the attraction number — but make it a launch discount rather than the
permanent price. Resolume Avenue sits at €299; pricing slightly under the
incumbent reads confident, far under reads hobby-project. The discount ending is
a marketing beat you get for free. Final digits are a launch-week call; nothing
else depends on them. Update-pass renewal price decided later (typical band is
40–50% of list). v2.0 is a paid upgrade for existing users.

## 4. How people try it

**Free forever, watermark on the video output until licensed.** Resolume's exact
model. No time limit, no crippled features — someone can learn the whole tool,
even gig with the watermark, and buy when it embarrasses them. No online DRM,
no aggressive anti-piracy: pirates were never customers, and paying users get
updates, support, and a license that can never fail them on stage (the license
check is fully offline — that's a selling point, say it out loud).

## 5. How people buy it

Use a **merchant of record** — Paddle or Lemon Squeezy. They are legally the
seller: they handle GST, EU VAT, US sales tax, invoices, and chargebacks
worldwide. You get a payout. Do not build your own store; that decision is
closed, not deferred.

Your real-world admin, in full:

- [ ] ABN (you likely have one — confirm it covers software sales).
- [ ] Apple Developer Program, $99 USD/yr — needed for code signing and
      notarization, no way around it. Do this early; builds need it.
- [ ] Pick Paddle or Lemon Squeezy, set up the product page.
- [ ] Register for GST once revenue passes $75k AUD.
- [ ] Accountant once money actually flows. Not before.

## 6. The five founding artists

Five VJs you already know, **free lifetime licenses, named "founding artists."**
Not a revenue tier — the value is watching someone else's first hour, which you
are structurally unable to see yourself. Rules of thumb:

- At least two of the five must actually **gig with it**, not poke at it at home.
  A show performed by someone-who-isn't-you kills the "only works for its
  author" doubt that hangs over every solo-built tool.
- Ask each for exactly two things: **where they got stuck in the first hour**,
  and **footage**.
- They seed the Discord and become the community's first answerers.

## 7. Marketing = your shows

You are the distribution. Two kinds of content with different jobs:

- **Gig footage** — "it looks fucking sick." Attracts attention.
- **Build videos** — "one person did this, in an hour." Converts. The
  screen-recording of dropping your .als and getting a show skeleton is the
  moment another artist decides to switch.

The discipline that starts **now**, not at launch: film every gig, screen-record
every build session. The launch reel gets assembled from shows that already
happened — you can't retro-shoot proof. One headline feature per release, each
with its own short video; everything else ships quietly in the changelog.

The first-hour experience is part of marketing: your target user opens MANIFOLD
already owning an `.als`. Import it, get a show scaffolded to their own music,
press play. No competitor can copy that first hour.

## 8. Support without drowning

The income is leveraged, not passive — the leverage is:

- **Docs stay agent-readable** (they already are), so the MCP agent doubles as
  first-line support inside the app.
- **Discord community** answers the community; the founding artists seed it.
- **Crash telemetry** (opt-in) turns field crashes into fixes without a ticket.
- **Release train, ~6–8 weeks, no promised dates.** Solo devs die by deadline,
  not workload.

Your job is triage and the occasional hard bug, not a support inbox.

## 9. What you'll realistically make

Honest sizing, recorded so nobody (including you) inflates it later: this is a
niche. Resolume serves the entire world's VJ scene with a small team. A
realistic arc is hundreds of seats early, low thousands over a couple of years —
at ~$299 that's real artist-income money, "passive compared to a real job," not
startup money. Every commitment above is sized for solo-plus-agents; nothing
assumes hiring.

## 10. Locked decisions / still open

**Locked** (don't re-litigate per release): the wedge positioning and four
pillars · recovery-demo marketing, never no-crash claims · the version map in §2
· perpetual + update pass · watermark trial · offline licensing · merchant of
record · five free lifetime cohort seats · one flex per release.

**Open, with their trigger:** final launch digits (launch week) · renewal
percentage (before first pass expires) · Resolume-campaign timing (when
stability + community can receive switchers) · Windows start date (gates on
v1.0 field stability) · venue/team licensing (when a venue actually asks).
