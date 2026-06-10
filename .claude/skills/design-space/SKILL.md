---
name: design-space
description: Author a new "space" for Kill the Serpent's level graph (a 2.5D corridor or an open 3D arena) following the per-space level-design checklist in issue #27 and the design pillars. Question-first — proposes 2-4 layout/pacing/encounter options with pillar + holistic-mixing reasoning, checks adjacent-space dependencies in the graph, and produces a per-space design spec. Use when designing a new level / area / arena.
---

# design-space

Level authoring for our **graph of authored spaces** (see #17 world structure +
#27). This is a *design-authoring* skill — it produces a space **spec** (the
input to whatever #27's space format becomes), not code. Question-first; the user
makes the creative calls.

## When to use
- Designing any new space: a capture arena, a 2.5D traversal corridor, a hub, a
  boss room.

## Load first
- `docs/design/PILLARS.md` (the three pillars + the holistic principle).
- #17 — the locked control model, the **per-space camera modes**
  (Follow / Rail2.5D / TopDown / OrbitSet / CaptureFraming), and the OQ register.
- #27 — the **per-space level-design checklist** + the space format.
- Existing spaces, to keep neighbors consistent.

## Process
1. **Clarify (ask first).** The space's role in the graph — **objective /
   obstacle / optional** capture, or pure traversal? The intended player
   experience? Its neighbors (which spaces connect in/out)? Reference feel?
2. **Camera mode drives layout.** Pick the per-space camera (open arena → Follow;
   platforming → Rail2.5D; etc.) *before* the layout — layout works within the
   framing, not against it.
3. **Propose 2-4 layout/pacing options**, each with:
   - pillar alignment (pen / pressure / few-tools-many-combos),
   - the **holistic test**: does this space *force mixing* systems (items ×
     tethers × environment × enemy state), or is it an isolated mode-room? Prefer
     mixing; if it must be single-system, justify it.
   - Recommend one; defer the choice to the user.
4. **Encounter + pacing.** Compose enemies via the shape-vulnerability matrix;
   place them on a pacing curve (arena = tension peak, corridor = rest).
5. **Wayfinding.** Landmarks / sightlines that *match the chosen camera framing*;
   keep vulnerability tells **shape-based, not color-only** (colorblind-safe by
   design — preserve that).
6. **Adjacent-space dependency check.** For every neighbor the layout references,
   confirm it exists. If not → mark the connection `UNRESOLVED` and list it; **do
   not invent** the neighbor's content.
7. **Accessibility.** Navigation clarity; no puzzle requiring >3 simultaneous
   states; handedness already handled by the input layer.
8. **Write the spec** (ask "May I write to …?" first) with acceptance criteria
   for the space.
9. **Verdict:** COMPLETE (spec written) or BLOCKED (list unresolved deps / open
   questions).

## After
- `design-guard` before implementing the space.
- `design-sync` to record any decisions made here back to the owning issue.
