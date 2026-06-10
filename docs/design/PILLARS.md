# Kill the Serpent — Design Pillars & Process

The north star. Read this first; then read the GitHub issues (the authoritative
design record — [#17](https://github.com/ChristopherJMiller/bevy-ds/issues/17) is the hub).

A DS-native capture game: a 3D cyber-dystopian reimagining of Pokémon Ranger's
loop-draw capture — a mechanic that *originated on this hardware*.

## Theme

You are a rebel in a post-singularity dystopia. A runaway AI superintelligence —
the **Serpent**, a nod to Roko's Basilisk — rules through an order of rogue
machines and punishes the civilization that birthed it. You fight the vast order
not by destroying its machines but by **hacking** them: tracing capture loops to
seize the Serpent's agents back from its control, one node at a time.
Anti-singularity, anti-capitalist — the small against the order. (This lean
toward *liberate, not destroy* informs, but does not yet lock, the capture-vs-
destroy open question in [#26](https://github.com/ChristopherJMiller/bevy-ds/issues/26).)

## The three pillars

Value-oriented, not feature lists. Every decision is filtered through these.
If a feature doesn't serve a pillar — or fights one — it's wrong, however cool.

1. **The pen is the power.** Every core verb flows through the stylus; precision
   drawing is the soul of the game. Locomotion, capture, item aiming — the
   dominant hand on the pen is *the* interface. Anything that demotes the stylus
   to a secondary input is suspect. (Why handedness is a hard requirement, not a
   menu toggle: the precision instrument must sit in the player's good hand.)

2. **Pressure is the puzzle.** Capture is a puzzle you solve *while* dodging —
   thought and threat at the same instant. Never a sitting duck (you always have
   evasive movement), never mindless action (the capture is a spatial problem).
   The two-screen split exists to make thinking and surviving simultaneous.

3. **Few tools, many combos.** A small verb set that *multiplies* — items ×
   tethers × environment × enemy state — rather than many verbs that each do one
   thing. **Never segmented modes.** This is the holistic principle below, and on
   this hardware it's not a preference — it's survival.

## The holistic principle (why pillar 3 is load-bearing)

The classic design-pillars trap is *functional* design: orient around discrete
systems and you build each in isolation, producing segmented play — "capture
mode," "item mode," "platforming mode" — that never interacts. The antidote is
**holistic / multiplicative** design: a few mechanics that combine in many ways
(think BotW's chemistry engine, or Crysis blending stealth and combat fluidly).

For a **solo dev on a 33 MHz ARM9 with ~4 MB RAM**, this is the only feasible
scope strategy. A game with a capture minigame + an item minigame + a platforming
minigame is *three games*. A game where the GDD's **item × tether × environment
synergies** are the *point* — where `bevy_nds_loop`, shape-vulnerability, and
item effects are cheap systems deliberately designed to multiply against each
other — is one game with depth.

**The test, applied to any new mechanic:** "Does this multiply against systems we
already have, or does it stand alone as its own mode?" Prefer the former. If
something must stand alone, that's a flag worth defending out loud.

## Process: feel before features

- **Prototype-first.** A prototype proves the core loop is *fun* with disposable
  code; a vertical slice proves that fun *survives* production constraints. Face
  the highest-uncertainty mechanics first. (This is why Milestone 1 is three
  feel-spikes — stylus locomotion, loop-draw, and the dual-screen fuse — before
  any systems work.)
- **Most of our core is host-testable pure math** (stick vector, loop geometry).
  Get the feel-critical logic under unit tests before ROM work.
- **Playtest with fresh eyes.** The reaction of someone who wasn't in the daily
  build matters more than our intention.
- **Every feel-spike ends in a verdict — PROCEED / PIVOT / KILL** — naming which
  open question it resolved and the next action. "Felt okay" is not a verdict.
  Capture it with `playtest-log` and record the consequence with `design-sync`.

## The issues are the design record

The GitHub issues on `ChristopherJMiller/bevy-ds` are authoritative — not chat
logs, not this file's prose, not memory.

- **Every design issue splits `## Locked` from `## Open questions`.** Locked is
  decided and may be built against. Open is genuinely unresolved and must NOT be
  asserted as fact anywhere.
- **Decisions made in conversation don't count until written back** to the owning
  issue. Use the `design-sync` skill.
- **Be honest about ambiguity.** If it isn't decided, it lives under Open
  questions. Inventing a false certainty is worse than naming the gap.

## Using the skills

- **Before building** a design-bearing issue → run **`design-guard`**: loads
  these pillars + the relevant issues, checks alignment and holistic fit, and
  stops to surface any blocking Open question before a line of code.
- **After a decision** → run **`design-sync`**: writes it back to the owning
  issue (Locked + rationale), updates the #17 hub, and keeps the unresolved
  things flagged Open. `design-sync --audit` sweeps for decisions discussed but
  never recorded, and for anything claimed Locked that's actually still Open.
