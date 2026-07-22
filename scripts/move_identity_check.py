#!/usr/bin/env python3
"""Move-identity verifier — the pure-move gate for the god-file decomposition waves.

A "pure move" commit relocates code without changing it. This script proves it
mechanically: it runs `git diff --color-moved` with pinned colors and counts
every added/removed line that git did NOT classify as moved. For a pure-move
commit the non-moved residue must be ZERO after the allowlist (module wiring:
`mod`/`use`/`pub use` lines, blank lines, module doc comments, diff headers,
and test-mod headers — `#[cfg(test)]` + `mod <name> {`/`}` — when tests are
distributed across the new submodules (D7a, 1-old→N-new) or one inline
`#[cfg(...)] mod X { … }` is converted to a `mod X;` declaration + sibling file
(W3-D2, 1-to-1; the `#[path = "…"]` tests-out form included)).
Exit code 0 = pure move proven; 1 = residue found (printed); 2 = usage.

Why not `cargo public-api`: not installed, requires a lib target (manifold-app
is bin-only — most of Wave 1 is invisible to it), and moving a pub item across
modules legitimately changes its path. This script sees every line instead.

Usage:
  python3 scripts/move_identity_check.py <commit>            # one commit vs its parent
  python3 scripts/move_identity_check.py <base>..<head>      # a range
  python3 scripts/move_identity_check.py --cached            # staged changes
  python3 scripts/move_identity_check.py <ref> --show-all    # print all residue lines

The moved-line detection uses `--color-moved=plain` with `--color-moved-ws=
ignore-all-space` so re-indented relocations still count as moves, and pins the
four diff colors explicitly so parsing never depends on user git config.
Blocks of fewer than 3 consecutive lines are below git's move-detection
threshold and will surface as residue — for a genuine tiny move, name it in the
commit message and the reviewer eyeballs those lines; that is the intended
manual surface, kept deliberately small.
"""

import re
import subprocess
import sys

# Pinned colors: 35=magenta (old moved), 36=cyan (new moved). git may emit
# them with attributes (e.g. \x1b[1;36m), so match the code anywhere in the
# leading escape sequence of the line.
MOVED_RE = re.compile(r"^\x1b\[(?:[0-9;]*;)?3[56](?:;[0-9;]*)?m")
ANSI = re.compile(r"\x1b\[[0-9;]*m")

# Module-wiring lines a pure move is allowed to add/remove.
ALLOW = re.compile(
    r"^[+-]\s*("
    r"$"                                        # blank
    r"|(pub(\((crate|super)\))?\s+)?mod\s+\w+;" # mod / pub mod decl
    r"|(pub(\((crate|super)\))?\s+)?use\s"      # use / pub use
    r"|#\[path\s*=\s*\".*\"\]"                  # #[path] attr on a mod decl
    r"|//!"                                     # module-level doc comment
    r"|impl\s+\w+\s*\{\s*$"                     # bare inherent-impl wrapper
    r")"
)
# The inherent-impl wrapper case (P-F2a, god-file wave): relocating methods into
# a submodule needs a fresh `impl UIRoot {` line the source never removed — pure
# structural scaffolding, behavior-neutral, exactly like the `mod`/`use` wiring
# above. Kept deliberately TIGHT: `impl <Name> {` on its own line only. A trait
# impl (`impl X for Y {`) has ` for ` after the name and does NOT match — it
# stays residue by design (a moved trait impl is rare enough to justify in
# review). A same-line body (`impl Foo { fn ... }`) fails the `\s*$` and stays
# residue too, so a smuggled edit can't hide behind the wrapper. The matching
# bare `}` is already picked up as a move by plain-mode (methods always carry
# removed `}` lines for it to pair against).
HEADER = re.compile(r"^(\+\+\+|---)\s")

# Comment-only lines: behavior-neutral in Rust (nextest does not run doctests
# in this repo's gate). Counted and reported, never fatal.
COMMENT = re.compile(r"^[+-]\s*(//|///|//!)")

# Visibility qualifiers a move may add/remove on an otherwise-identical line —
# widening is required wiring when a private item crosses a module wall, and
# cannot change runtime behavior (it only widens who may call).
VIS = re.compile(r"^(pub(\((crate|super)\))?\s+)")

# Multi-line `use { ... }` brace-list continuation (D-18): the single-line
# ALLOW regex above only matches lines that themselves start with `use`, so
# the inner list lines and the closing `};` of a multi-line import block were
# surfacing as residue even though they are pure wiring. Fixed with STATEFUL
# per-sign block tracking in `classify()`: a +/- line matching USE_OPEN below
# (a `use` line ending in an unclosed `{`) opens a block for that sign; while
# open, a same-sign line is allowed ONLY if it matches USE_ITEM — one or more
# `ident`/`a::b::c` path items, comma-separated, optional `as alias`, optional
# trailing comma, optional closing `};` (which closes the block). Anything
# else inside an open block is NOT import-shaped and falls through to residue
# — this is the smuggle-proofing: a real code statement hidden between the
# `use x::{` opener and the `};` closer must still be caught. Per-sign state
# resets on every non-content diff line (hunk/file header, context line) so a
# stray unrelated line elsewhere in the diff can never inherit an open block —
# UNLESS that context line is itself a `use ... {` opener (D-20 i, see below).
#
# Context-opened blocks (D-20 i): the above only ARMS on a SIGNED opening
# line. An inner-line-only edit under an otherwise-UNCHANGED `use ... {` —
# e.g. one name swapped in a pre-existing multi-line import, opener and
# closer both untouched — never emits the opener as a +/- line, so per-sign
# tracking never arms and the inner +/- lines fell to residue. Fixed with a
# second, SHARED `context_block` flag in `classify()`: a context (unchanged)
# line matching USE_OPEN arms it; while armed it governs BOTH +/- inner
# lines (the opener applies to old and new file alike); a context line
# matching the closer shape disarms it. USE_ITEM's shape check is untouched
# and applies identically to context-armed and signed-armed blocks, so the
# smuggle-proofing (a non-import statement inside an open block is still
# residue) holds for both.
USE_OPEN = re.compile(r"^(pub(\((crate|super)\))?\s+)?use\s.*\{\s*$")
USE_CLOSE = re.compile(r"^\}\s*;?$")
_IDENT = r"[A-Za-z_]\w*"
_PATH_ITEM = rf"{_IDENT}(?:::{_IDENT})*(?:\s+as\s+{_IDENT})?"
USE_ITEM = re.compile(
    rf"^(?:{_PATH_ITEM}(?:\s*,\s*{_PATH_ITEM})*,?\s*(?:\}}\s*;?)?|\}}\s*;?)$"
)

# Test-mod wiring class (D7a, Wave 2 P2-G): distributing one flat
# `#[cfg(test)] mod tests { ... }` into ~7 per-module test mods multiplies the
# header lines — the cfg-test attribute, the `mod <name> {` opener, and the
# closing `}`. git's `--color-moved` USUALLY pairs each repeated header against
# the single removed original, but that is threshold-fragile: when the new mod
# is renamed or its cfg gate differs from the original (e.g. a feature-gated
# `#[cfg(all(test, feature = "…"))]`), the added header line has no identical
# removed counterpart and falls to residue even though it is pure wiring
# (verified: a renamed/feature-gated distribution surfaces exactly the cfg-attr
# and `mod {` opener lines as residue). This class ALLOWS them, SMUGGLE-PROOF:
# the cfg attr is wiring only when the IMMEDIATELY-following same-sign line is a
# `mod <name> {` opener (the attribute must be attached to a mod), and a bare
# `}` is wiring only while a counted test-mod brace is open — any other line
# under the class (a smuggled statement in a test-mod header) falls straight
# through to residue. State + matching live in classify() (per-sign, reset at
# every hunk/file/context boundary like the use-block trackers). The `}` depth
# advance is deliberately confined to NON-moved lines: the distributed test
# BODIES are git-detected moves and never reach it, so their internal braces
# can't desync the counter.
CFG_TEST_ATTR = re.compile(
    r'^#\[cfg\((?:test|all\(\s*test\s*,\s*feature\s*=\s*"[^"]*"\s*\))\)\]$'
)
MOD_OPEN = re.compile(r"^(?:pub(?:\((?:crate|super)\))?\s+)?mod\s+\w+\s*\{$")
BARE_CLOSE = re.compile(r"^\}$")

# Dispatch-split scaffold (UI_FUNNEL_DECOMPOSITION P-B): the structural glue a
# `dispatch_inspector` match-split adds that git cannot classify as a move
# because it is genuinely new text, not relocated — a sub-dispatcher signature,
# its `match action {`, the `unhandled` sentinel, closing braces, and the
# ordered first-non-unhandled CHAIN ROUTER lines (one delegation call + one
# fall-through guard per domain module — bounded by module count, ~2 lines
# each). Counted SEPARATELY and capped (SCAFFOLD_CAP): bulk semantics must
# never hide here, so a commit exceeding the cap FAILS. Deliberately NARROW —
# no delegation-arm or `PanelAction::` pattern of any kind, because a
# hand-written variant→module arm is a routing decision (its correctness is
# proven by variant-census equality, NOT waived here). The chain-router lines
# are per-DOMAIN (~7 total), not per-variant, so they stay well under the cap
# and cannot smuggle a misroute. See docs/UI_FUNNEL_DECOMPOSITION_DESIGN.md
# D6 / INV-G1.
SCAFFOLD_CAP = 25
SCAFFOLD = re.compile(
    r"^[+-]\s*(?:"
    r"(?:pub(?:\((?:crate|super)\))?\s+)?fn dispatch_\w+\(.*"  # (1) sub-dispatcher signature (single line)
    r"|match action \{"                                        # (2) the per-domain match head
    r"|_ => DispatchResult::unhandled\(\),?"                   # (3) the fall-through sentinel
    r"|DispatchResult::unhandled\(\)"                          # (3b) bare tail expr: router
                                                                #      fully collapsed (S6b)
    r"|let \w+ = [\w:]+::dispatch_\w+\(action, ctx\);"         # (5) chain-router delegation call
    r"|if !(\w+)\.unhandled \{ return \1; \}"                  # (6) chain-router fall-through guard
    r"|\}\)?,?;?"                                              # (4) a bare closing brace
    r")\s*$"
)

# D-11 preamble (UI_FUNNEL_DECOMPOSITION P-B, params/modulation/mapping domains): the
# ONLY sanctioned preamble is byte-exact. When a domain is split into its own
# `dispatch_<d>` fn, that fn can't inherit the outer fn's locals and must recompute
# these two lines at its top; later slices delete the now-dead original from
# inspector.rs. So these lines legitimately appear as `+` (new fn) and eventually `-`
# (dead original) with zero behavior change — counted as SCAFFOLD (same SCAFFOLD_CAP),
# never residue. Matched as EXACT-STRING literals, whitespace-normalized (leading
# +/- marker stripped, internal whitespace collapsed to single spaces), deliberately
# NARROW — NOT a general `let ... = super::...` shape. Any deviation (different arg,
# renamed local, reordered call) is NOT in this set and falls straight through to
# residue: "any deviation from the byte-exact form = residue = investigate, never
# adapt" (D-11). Encodes D-11's text as the truth, not whatever inspector.rs happens
# to contain today.
#
# Drifted removal-side SEQUENCE (D-21, replacing D-20 iii's per-line entries):
# inspector.rs's ACTUAL in-source preamble (still in `dispatch_inspector`,
# verified against the source at the time of the D-20 iii fix) drifted from
# the canonical form above — an explicit `&*ctx.active_layer` reborrow on the
# call's last arg, an explicit `&Option<LayerId>` type annotation on the
# second `let`, and the call formatted across multiple lines rather than one.
#
# D-20 iii originally registered each drifted source line as an independent
# PREAMBLE_LINES member. That over-generalized: several of those lines are
# short generics (`");"`, `"ctx.editor_target,"`, `"&*ctx.project,"`) that, as
# permanent standalone entries, mask ANY genuinely-deleted matching line in
# ALL future commits — e.g. a dropped match arm's call-closer `");"` would
# silently vanish from its residue signature instead of being caught. D-21
# fixes this: the drifted lines are kept as an ORDERED SEQUENCE, and
# classify() below matches them with a stateful REMOVAL-SIDE-only tracker —
# armed only by the exact opener, advanced only by the exact next line in
# order, disarmed (mismatch falls to residue) the instant a line breaks the
# chain. A `");"` (or any other member of this sequence) seen out of order or
# in isolation is no longer scaffold — it is caught as residue, same as any
# other genuinely deleted line.
PREAMBLE_LINES = {
    "let (effective_tab, effective_active_layer) = super::editor_dispatch_context"
    "(ctx.editor_target, &*ctx.project, ctx.ui.inspector.last_effect_tab(), "
    "ctx.active_layer);",
    "let active_layer = &effective_active_layer;",
}
# The exact ordered drifted sequence (opener first, through the closer), one
# physical source line each — git emits each physical line of a removed
# multi-line statement as its own `-` line, which is why this is a sequence
# of lines rather than one joined statement like the canonical form above.
# REMOVAL-side only: when the LAST preamble-using domain moves out, the
# drifted original is deleted with nothing left behind to inherit it (the new
# location recomputes the CANONICAL form, matched above), so this sequence
# only ever needs to match `-` diff lines, never `+`.
DRIFTED_PREAMBLE_SEQUENCE = (
    "let (effective_tab, effective_active_layer) = super::editor_dispatch_context(",
    "ctx.editor_target,",
    "&*ctx.project,",
    "ctx.ui.inspector.last_effect_tab(),",
    "&*ctx.active_layer,",
    ");",
    "let active_layer: &Option<LayerId> = &effective_active_layer;",
)


def _normalize_ws(body: str) -> str:
    """Collapse internal whitespace to single spaces for exact-string comparison."""
    return " ".join(body.split())


def drop_visibility_pairs(residue: list[str]) -> tuple[list[str], int]:
    """Remove matched -old/+new pairs that are identical after stripping a
    leading visibility qualifier from the line's code (post +/- marker,
    whitespace-insensitive). Returns (remaining residue, pairs dropped)."""

    def key(line: str) -> str:
        body = line[1:].lstrip()
        return " ".join(VIS.sub("", body, count=1).split())

    minus: dict[str, int] = {}
    plus: dict[str, int] = {}
    for line in residue:
        (minus if line.startswith("-") else plus)[key(line)] = (
            (minus if line.startswith("-") else plus).get(key(line), 0) + 1
        )
    pairs = 0
    remaining: list[str] = []
    # Two passes so ordering inside the diff doesn't matter: count matches,
    # then emit unmatched lines in original order.
    matched: dict[str, int] = {}
    for k in minus:
        m = min(minus[k], plus.get(k, 0))
        if m:
            matched[k] = m
            pairs += m
    spent: dict[tuple[str, str], int] = {}
    for line in residue:
        k = key(line)
        side = "-" if line.startswith("-") else "+"
        if matched.get(k, 0) > spent.get((k, side), 0):
            spent[(k, side)] = spent.get((k, side), 0) + 1
            continue
        remaining.append(line)
    return remaining, pairs


# include_str! path-depth prefix rewrite (D6, Wave 3): moving a test mod DEEPER
# into a directory module (freeze/codegen.rs's `gpu_tests` -> freeze/codegen/
# gpu_tests.rs, one level; preset_runtime.rs's test mods -> preset_runtime/
# tests/*.rs, two levels) makes every relative `include_str!("../…")` argument
# resolve from a deeper directory, so its leading `../` run must grow by exactly
# the added nesting depth. That is the ONLY content change a test-mod relocation
# legitimately forces onto a moved line — every other byte of the line is
# preserved. This class pairs a removed line against an added line that are
# identical after collapsing the LEADING `../` run of every `include_str!`
# argument on the line (whitespace-insensitive); ANYTHING else different — the
# path tail, the surrounding code, a second literal — breaks the pair and both
# lines fall to residue. SMUGGLE-PROOF: a changed shader/asset path, or any code
# edit sharing the line, changes the normalized key and is caught. Only lines
# CONTAINING `include_str!` participate; every other residue line passes through
# untouched, so the class can never mask an unrelated deletion. Leading run
# only: a `../` appearing mid-path (not right after the opening quote) is part
# of the tail and is NOT collapsed — changing it is caught.
INCLUDE_STR_LEADING_DOTDOT = re.compile(r'(include_str!\s*\(\s*")(?:\.\./)+')


def drop_include_str_prefix_pairs(residue: list[str]) -> tuple[list[str], int]:
    """Remove matched -old/+new pairs of `include_str!` lines that are identical
    after collapsing the leading `../` run of every include_str! argument on the
    line (post +/- marker, whitespace-insensitive). Only lines containing
    `include_str!` participate. Returns (remaining residue, pairs dropped)."""

    def key(line: str) -> str:
        body = INCLUDE_STR_LEADING_DOTDOT.sub(r"\1", line[1:])
        return " ".join(body.split())

    minus: dict[str, int] = {}
    plus: dict[str, int] = {}
    for line in residue:
        if "include_str!" not in line:
            continue
        bucket = minus if line.startswith("-") else plus
        bucket[key(line)] = bucket.get(key(line), 0) + 1
    matched: dict[str, int] = {}
    pairs = 0
    for k in minus:
        m = min(minus[k], plus.get(k, 0))
        if m:
            matched[k] = m
            pairs += m
    remaining: list[str] = []
    spent: dict[tuple[str, str], int] = {}
    for line in residue:
        if "include_str!" not in line:
            remaining.append(line)
            continue
        k = key(line)
        side = "-" if line.startswith("-") else "+"
        if matched.get(k, 0) > spent.get((k, side), 0):
            spent[(k, side)] = spent.get((k, side), 0) + 1
            continue
        remaining.append(line)
    return remaining, pairs


def classify(out: str) -> tuple[dict[str, int], list[str]]:
    """Bucket every +/- line of a `--color-moved` diff into moved / allowlisted
    wiring / comment / dispatch-split scaffold, returning (counts, residue).
    Split out of `main` so the self-test can feed synthetic colored diffs
    without constructing a git repo."""
    residue: list[str] = []
    counts = {"moved": 0, "allowed": 0, "comments": 0, "scaffold": 0}
    # Per-sign use-block state (D-18): True while a `-` (resp. `+`) multi-line
    # `use { ... }` opened by a SIGNED line is open and hasn't hit its
    # closing `};` yet.
    open_block = {"+": False, "-": False}
    # Context-opened use-block state (D-20 i): True while a multi-line
    # `use { ... }` whose OPENER is an UNCHANGED (context) line is open. This
    # is a single SHARED flag, not per-sign — the opener is unchanged so it
    # applies to both the old and new file, and governs +/- inner lines of
    # either sign until a context (unchanged) closer disarms it.
    context_block = False
    # Context-opened test-mod cfg state (W3-D4, the D-20(i) analog of
    # pending_test_attr): a CONTEXT (unchanged) `#[cfg(test)]`/feature-gated cfg
    # line arms the following signed `mod X {` opener as wiring. git keeps the
    # cfg line as context (not a signed self-move) when a RUN of consecutive
    # inline test mods is converted to `#[cfg] #[path="…"] mod X;` decls at once
    # (P3-R's e09e078b, 11/11): git's minimal diff anchors the identical cfg
    # lines as context and only diffs the mod lines, so pending_test_attr (which
    # only arms off a SIGNED cfg line) never fires. Shared, not per-sign — the
    # cfg is unchanged so it applies to both old and new. One-line lifetime: set
    # by a context cfg line, consumed by the immediately-following signed line.
    context_test_attr = False
    # Drifted-preamble removal-side sequence state (D-21): index of the NEXT
    # expected line in DRIFTED_PREAMBLE_SEQUENCE, or None while disarmed.
    # Removal-side only, so unlike open_block it is not per-sign. Reset
    # everywhere the other block trackers reset — a stray later `-");"`
    # elsewhere in the diff must never inherit an armed sequence.
    drifted_idx: int | None = None
    # Test-mod wiring state (D7a): pending_test_attr[sign] is True for exactly
    # the one line following a cfg-test attribute of that sign (only a `mod {`
    # opener there is wiring); test_mod_depth[sign] counts open test-mod braces
    # so a bare `}` closing one is wiring. Reset with the other trackers.
    pending_test_attr = {"+": False, "-": False}
    test_mod_depth = {"+": 0, "-": 0}
    for raw in out.splitlines():
        is_moved = bool(MOVED_RE.match(raw))
        plain = ANSI.sub("", raw)
        if HEADER.match(plain):
            # Diff file header (+++ / ---): never content, and a use block
            # can never legitimately span one.
            open_block["+"] = False
            open_block["-"] = False
            context_block = False
            context_test_attr = False
            drifted_idx = None
            pending_test_attr["+"] = False
            pending_test_attr["-"] = False
            test_mod_depth["+"] = 0
            test_mod_depth["-"] = 0
            continue
        if not plain.startswith(("+", "-")):
            # Hunk header (`@@ ... @@`) or real unchanged context line. A
            # signed block can't legitimately span either, so that state
            # always resets here.
            open_block["+"] = False
            open_block["-"] = False
            drifted_idx = None
            pending_test_attr["+"] = False
            pending_test_attr["-"] = False
            test_mod_depth["+"] = 0
            test_mod_depth["-"] = 0
            context_test_attr = False
            if plain.startswith("@"):
                # Hunk boundary: never content, and a context-opened block
                # can't legitimately span one either — two unrelated use
                # blocks in different hunks must never be treated as one.
                context_block = False
                continue
            # Real context line — it may be the opener or closer of a
            # context-opened block (D-20 i), so check before dropping it.
            body = plain[1:].strip() if plain else ""
            if context_block:
                if USE_CLOSE.match(body):
                    context_block = False
            elif USE_OPEN.match(body):
                context_block = True
            # A context `#[cfg(test)]`/feature-gated cfg line arms the following
            # signed `mod X {` opener (W3-D4); any other context line disarms it
            # (one-line lifetime, mirroring the signed pending_test_attr arm).
            context_test_attr = bool(CFG_TEST_ATTR.match(body))
            continue
        sign = plain[0]
        if sign == "+":
            # The drifted sequence is removal-side only: a `+` line can never
            # arm, advance, or belong to it, and it breaks any run in
            # progress (the removed lines a chain tracks are no longer
            # contiguous once an addition interrupts them).
            drifted_idx = None
        # Drifted-preamble removal-side sequence MATCH/ADVANCE (D-21, S5b
        # fix): computed BEFORE the is_moved short-circuit below, for every
        # removal-side line unconditionally. git's move detector works on
        # CONTENT identity alone — it flags a drifted removal line "moved"
        # whenever some other hunk happens to add an identical line
        # elsewhere (e.g. the short generic `");"` closer, which recurs
        # verbatim in code that moved to a sibling module — confirmed
        # against fb59db17's real diff). If the tracker were only consulted
        # after the is_moved continue (as it was pre-S5b), such a line would
        # be counted moved and skip the tracker entirely, leaving drifted_idx
        # one step behind for every subsequent line — desyncing the sequence
        # and surfacing a genuinely-dead later line as residue. Checking the
        # match here, ahead of is_moved, means a matching line ADVANCES the
        # tracker regardless of its moved-flag, keeping drifted_idx's
        # position a function of the CONTENT sequence of removal-side lines,
        # not of git's moved-flag.
        #
        # DISARM-on-mismatch deliberately stays OUT of this block (unlike the
        # pre-S5b single-site version) and is decided below instead, at the
        # original fallthrough site, AFTER open_block/ALLOW/COMMENT/
        # PREAMBLE_LINES/SCAFFOLD have all had a chance to claim the line.
        # Reason (also confirmed against fb59db17's real diff): the actual
        # drifted preamble in inspector.rs has two ordinary comment lines
        # ("// No arm mutates …") sitting between the sequence's `);` closer
        # and its final `let active_layer: …` line — comment lines that were
        # ALWAYS transparent to this tracker pre-S5b, because they are
        # consumed by the COMMENT check (below) and `continue` before ever
        # reaching the old single-site drifted check. Disarming here
        # unconditionally on every non-matching removal line (including
        # those comments) would break that transparency and reproduce a
        # residue-1 regression of its own. So: ADVANCE is unconditional and
        # happens here; DISARM only fires for a line that reaches the
        # bottom of the classification chain without being claimed by
        # anything else — exactly where it fired pre-S5b.
        #
        # ADVANCE is decoupled from CLASSIFICATION either way: this block
        # only updates drifted_idx and records whether this line matched
        # (drifted_match) — it does NOT touch counts. The line's bucket is
        # decided further down: a moved line stays moved (the is_moved
        # continue right here, untouched, no double-count); a non-moved
        # matched line becomes scaffold at the original site; a non-moved
        # unmatched line that reaches the bottom disarms the tracker there
        # and falls to residue — the smuggle-proof catch (a lone
        # out-of-sequence `");"` etc. is still residue) is unchanged.
        drifted_match = False
        if sign == "-":
            body = _normalize_ws(plain[1:])
            if drifted_idx is not None and body == DRIFTED_PREAMBLE_SEQUENCE[drifted_idx]:
                drifted_match = True
                drifted_idx += 1
                if drifted_idx == len(DRIFTED_PREAMBLE_SEQUENCE):
                    drifted_idx = None  # sequence complete: disarm
            elif body == DRIFTED_PREAMBLE_SEQUENCE[0]:
                # Either a fresh arm, or a mismatch that happens to be a new
                # opener — re-arm on it either way.
                drifted_match = True
                drifted_idx = 1
        # Test-mod cfg-attr ARM — BEFORE the is_moved short-circuit (W3-D2, the
        # same "advance ahead of is_moved" idiom the drifted-preamble tracker
        # above uses, and for the same reason). D7a arms `pending_test_attr` off
        # a `#[cfg(test)]`/feature-gated cfg attribute so the `mod X {` opener on
        # the immediately-following same-sign line is recognized as wiring. In a
        # 1-old→N-new test distribution the added cfg attrs have no removed
        # counterpart, so they are non-moved and the old post-is_moved arming
        # sufficed. But converting ONE inline `#[cfg(...)] mod X { … }` into a
        # `mod X;` declaration + sibling file (W3-D2: P3-C's tests.rs/gpu_tests,
        # P3-R's 11 #[path] tests-out decls) RE-ADDS the identical cfg line
        # 1-to-1, and git's --color-moved pairs it as a self-move — so it would
        # short-circuit as `moved` below before ever arming, and the following
        # `-mod X {` opener would fall to false residue (P3-C range: exactly the
        # two `-mod dispatch_contract_tests {` / `-mod gpu_tests {` lines).
        # Arming here fixes the 1-to-1 case and is a no-op for the 1-to-N case.
        # `was_test_attr` captures the PREVIOUS same-sign line's arm; the flag is
        # then re-derived from whether THIS line is itself a cfg-test attr. A
        # moved cfg line still arms the next line but is still bucketed as moved
        # below (no double count); the arm survives only to the immediately-next
        # same-sign line (any other line, moved or not, re-derives it) — so a
        # non-`mod {` line after a cfg attr can never be smuggled in as wiring.
        tm_body = plain[1:].strip()
        # Consume either arm: a same-sign SIGNED cfg attr (pending_test_attr) or
        # a shared CONTEXT cfg attr (context_test_attr, W3-D4). Both live only to
        # this immediately-following signed line; clear the context arm now.
        was_test_attr = pending_test_attr[sign] or context_test_attr
        context_test_attr = False
        pending_test_attr[sign] = bool(CFG_TEST_ATTR.match(tm_body))
        # Use-block opener ARM — BEFORE the is_moved short-circuit (W3-D3, the
        # use-block sibling of the test-mod fix above; same root cause). D-18
        # arms `open_block[sign]` off a `use …::{` opener so the block's item
        # continuation lines are recognized as wiring — but that arming lives in
        # the ALLOW branch BELOW the is_moved short-circuit. When a directory
        # split redistributes one combined multi-line `use path::{ … }` import
        # across several sibling modules, the identical `use path::{` opener text
        # recurs on both the removed (1×) and added (N×) sides, so git's
        # --color-moved=plain flags every opener as a MOVE — it short-circuits as
        # `moved` before ALLOW ever arms the block, and the RE-GROUPED item lines
        # (regrouped across physical lines, so none move-pair) fall to false
        # residue (P3-G: the manifold_core::effect_graph_def redistribution).
        # Arming here, ahead of is_moved, opens the block regardless of the
        # opener's moved-flag. Smuggle-proofing unchanged: only USE_ITEM-shaped
        # lines inside an open block are waived (a real statement still falls to
        # residue), and a hunk/context boundary still resets the block.
        if USE_OPEN.match(tm_body):
            open_block[sign] = True
        if is_moved:
            counts["moved"] += 1
            continue
        # Test-mod wiring header shapes (D7a; non-moved lines only — moved test
        # bodies short-circuited above, so their internal braces never reach the
        # depth counter). The cfg attr itself is wiring; a `mod X {` opener right
        # after one opens a counted brace; a bare `}` closing a counted brace is
        # wiring. Any other line under the class falls through to residue
        # (smuggle-proof). `pending_test_attr` was already armed above.
        if CFG_TEST_ATTR.match(tm_body):
            counts["allowed"] += 1
            continue
        if was_test_attr and MOD_OPEN.match(tm_body):
            counts["allowed"] += 1
            test_mod_depth[sign] += 1
            continue
        if test_mod_depth[sign] > 0 and BARE_CLOSE.match(tm_body):
            counts["allowed"] += 1
            test_mod_depth[sign] -= 1
            continue
        if open_block[sign] or context_block:
            body = plain[1:].strip()
            if USE_ITEM.match(body):
                counts["allowed"] += 1
                if "}" in body:
                    open_block[sign] = False
                    context_block = False
                continue
            # Not import-item-shaped: a real line was smuggled inside the
            # open block (signed or context-opened alike). Leave the block
            # "open" (a well-formed diff will still close it later) and fall
            # through to residue below.
        if ALLOW.match(plain):
            counts["allowed"] += 1
            if USE_OPEN.match(plain[1:].strip()):
                open_block[sign] = True
            continue
        if COMMENT.match(plain):
            counts["comments"] += 1
            continue
        if _normalize_ws(plain[1:]) in PREAMBLE_LINES:
            counts["scaffold"] += 1
            continue
        if SCAFFOLD.match(plain):
            counts["scaffold"] += 1
            continue
        if sign == "-":
            # Reaching here means this line is NOT moved, and none of
            # open_block/ALLOW/COMMENT/PREAMBLE_LINES/SCAFFOLD claimed it —
            # the original fallthrough site (D-21), now driven by the
            # match already computed above (S5b) rather than re-deriving it.
            if drifted_match:
                counts["scaffold"] += 1
                continue
            # DISARM here, not in the pre-is_moved block above: this is the
            # instant a removal-side line breaks the chain FOR REAL (it
            # wasn't absorbed as comment/wiring/scaffold either), so the next
            # line must not inherit a stale armed sequence — exactly the
            # pre-S5b disarm-on-mismatch behavior, just relocated to keep
            # comment/wiring/scaffold lines transparent to the tracker (see
            # the comment above).
            drifted_idx = None
        residue.append(plain)
    return counts, residue


def main() -> int:
    args = [a for a in sys.argv[1:] if a != "--show-all"]
    show_all = "--show-all" in sys.argv
    if len(args) != 1:
        print(__doc__)
        return 2
    target = args[0]

    diff_args = [
        "git",
        "-c", "color.diff.oldMoved=magenta",
        "-c", "color.diff.newMoved=cyan",
        "-c", "color.diff.old=red",
        "-c", "color.diff.new=green",
        "diff",
        "--color=always",
        "--color-moved=plain",
        "--color-moved-ws=ignore-all-space",
    ]
    if target == "--cached":
        diff_args.append("--cached")
    elif ".." in target:
        diff_args.append(target)
    else:
        diff_args.append(f"{target}^!")

    out = subprocess.run(diff_args, capture_output=True, text=True, check=True).stdout

    counts, residue = classify(out)
    residue, vis_pairs = drop_visibility_pairs(residue)
    residue, include_pairs = drop_include_str_prefix_pairs(residue)
    scaffold = counts["scaffold"]

    print(
        f"moved lines: {counts['moved']}  allowlisted wiring: {counts['allowed']}  "
        f"comment lines: {counts['comments']}  scaffold: {scaffold}  "
        f"visibility pairs: {vis_pairs}  include_str pairs: {include_pairs}  "
        f"residue: {len(residue)}"
    )
    if vis_pairs:
        print(f"  note: {vis_pairs} signature(s) widened visibility (fn -> pub(crate) fn "
              f"etc.) — required wiring when private items move across module walls.")
    if include_pairs:
        print(f"  note: {include_pairs} include_str! path(s) had their leading '../' depth "
              f"rewritten — required wiring when a test mod moves deeper (D6).")
    if scaffold > SCAFFOLD_CAP:
        print(f"  scaffold {scaffold} EXCEEDS cap {SCAFFOLD_CAP}: a dispatch-split commit")
        print("  may not carry this many structural lines — bulk semantics must not hide")
        print("  in scaffold. Split into smaller slices (fewer domain modules per commit).")
        return 1
    if residue:
        limit = len(residue) if show_all else 40
        for line in residue[:limit]:
            print(f"  RESIDUE {line}")
        if len(residue) > limit:
            print(f"  … {len(residue) - limit} more (--show-all to print)")
        print("NOT a pure move. Residue lines are semantic changes or sub-threshold")
        print("(<3-line) moves — split the commit or justify each line in review.")
        return 1
    if scaffold:
        print(f"PURE MOVE PROVEN: every non-scaffold changed line is a detected move "
              f"({scaffold} dispatch-split scaffold line(s), within cap {SCAFFOLD_CAP}).")
    else:
        print("PURE MOVE PROVEN: every non-wiring changed line is a detected move.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
