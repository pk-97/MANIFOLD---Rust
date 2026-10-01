# Agent Routing — provider lanes and the Kimi profile (retired)

**Status:** RETIRED 2026-10-01. Peter moved the rig to Claude models only. Kept because old hooks and design docs cite "Native provider lanes"; the live policy is `docs/AGENT_ROUTING.md`. Frozen, never edit.

What follows is the text removed from `docs/AGENT_ROUTING.md` that day, verbatim apart from section pointers.

## Launch profiles (as of 2026-09-29)

Which model a slot name reaches depended on how the top session was launched.

| Profile | Launch | Lead | `fable` | `opus` | `sonnet` | `haiku` |
|---|---|---|---|---|---|---|
| Anthropic | `claude` / `claude-m` | Fable 5.1 or Opus 5.5 | Fable | Opus | Sonnet 5.5 | Haiku |
| Kimi | `k3m` (via the litellm proxy) | Kimi K3 | K3 | K2.7 | K2.7 — classifier slot, never lanes | K2.7 |

Kimi-profile mechanics (proxy, slot remapping, the `k27-*` lane label): section Native provider lanes below. Provider operations and the proxy runbook: `docs/archive/PROVIDER_OPERATIONS.md`.

## The tiering, Kimi column

| Seat | Kimi profile |
|---|---|
| Lead | K3 |
| Landing seat | none, the lead lands |
| Lane executor | K2.7 (`haiku`; `opus` = same model) |
| Consult | K3 fork |
| Auto-approval classifier | K2.7 on the `sonnet` slot. Not a lane. If the Kimi cap bites: `seat_tool.py assign sonnet <model>`. |

Kimi profile, landing seat: the `sonnet` slot is the classifier, so the lead lands itself.

Kimi profile, consult contract: `cc-fleet subagent kimi --prompt-file <brief> --profile slim-ro --background`.

K3 serves at roughly 30–55 tokens/s, so on the Kimi profile the lead plans tersely: no narration before spawning, briefs and verdicts only.

## Native provider lanes

Kimi profile only. Lanes are ordinary Agent-tool subagents; the seat's env routes every API call through the litellm proxy (127.0.0.1:4000) and remaps the four model slots, so `model: "haiku"` is a K2.7 lane, steerable via SendMessage and itemized in the proxy ledger. `k3m` pins the lead to K3 via `ANTHROPIC_MODEL=k3`; slot config can never change the lead. Invariant: no two seats share a strong slot (`seat-identity.py`). Lane self-reports about identity are theater; the proxy log is the oracle for which model served a request. Mechanics and upgrade hazards: the `litellm-seat-proxy` memory and `docs/archive/PROVIDER_OPERATIONS.md`.

`cc-fleet spawn` (tmux teammates) is denied for every tier (`cc-fleet-tier-guard.py`): headless lanes are invisible in the UI and unreachable by SendMessage. `cc-fleet subagent` one-shots remain for consults; `cc-fleet run` remains the lead's launcher.

Kimi gotcha: `kimi-for-coding` on the endpoint is K2.7, not K3. Kimi bills cache reads, which are ~90% of lane volume, so K3 is only cheap against the flat plan window.
