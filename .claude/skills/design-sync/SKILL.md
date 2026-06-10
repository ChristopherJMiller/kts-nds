---
name: design-sync
description: Keep the Kill the Serpent design record authoritative and honest. Use AFTER a design decision is made (in conversation or while building) to write it back to the owning GitHub issue — move the item to ## Locked with a dated rationale, update the #17 hub's Open-questions register and tracking index, and keep anything still unresolved flagged under ## Open questions. Run with "--audit" to sweep all design issues for decisions discussed but never recorded, and for anything claimed Locked that is actually still open.
---

# design-sync

A decision made in conversation **does not count** until it's written to the
owning issue. This skill keeps the GitHub board (`ChristopherJMiller/bevy-ds`)
the single source of truth, and keeps it *honest* — locked things are decided,
open things are visibly open, and nothing pretends to be settled that isn't.

## The convention

Every design issue carries two structural sections:
- `## Locked` — decided. May be built against. Each line ends with
  `(decided YYYY-MM-DD: <one-line rationale>)`. **Use absolute dates.**
- `## Open questions` — genuinely unresolved. Must NOT be asserted as fact
  anywhere else (code, docs, other issues).

[[#17]] additionally holds the **repo-wide Open-questions register** (`OQ-n`,
each pointing at its home issue) and the milestone/epic **tracking index**.

## Mode A — record a decision

1. **Identify the owning issue** for the decision.
2. `gh issue view <n> --repo ChristopherJMiller/bevy-ds` to get the current body.
3. Edit the body:
   - Add the decision to `## Locked` with a dated rationale.
   - Remove (or strike through) the now-resolved item from `## Open questions`.
4. `gh issue edit <n> --repo ChristopherJMiller/bevy-ds --body-file <tmp>`.
5. **Update the hub (#17):** resolve the matching `OQ-n` in the register (strike
   it, note where/when decided), and tick the tracking index if a piece shipped.
6. If the decision changes the **locked control model**, reflect it in #17's
   `## Locked` too — the control model lives there.

## Mode B — `--audit` (honesty sweep)

1. `gh issue list --repo ChristopherJMiller/bevy-ds --label design --label epic --label spike --label vision --state open` (run per-label as needed).
2. For each issue, verify it has **both** `## Locked` and `## Open questions`
   sections. If an older issue has design notes inline, migrate them into the
   split (locked facts → Locked; hedges/"TBD"/"open" → Open questions).
3. **Cross-check against the recent conversation:** was any decision agreed in
   chat but never written to an issue? List each one as a gap to close (then run
   Mode A on it).
4. **Reverse-check:** is anything under `## Locked` actually contested, hedged,
   or since reopened? Flag it to move back to `## Open questions` — a false lock
   is worse than an honest open.
5. **Report** the findings first; apply fixes with `gh issue edit` only after the
   user confirms.

## Mode C — `--propagate <issue#>` (holistic consistency)

When a `## Locked` decision in an issue *changes* (not just gets added),
downstream work built against the old assumption may be stale. Find and flag it.

1. `gh issue view <n>` and read the changed Locked decision; if useful, diff
   against the prior version of any committed doc that mirrors it.
2. Find dependents: scan all issues for `Depends on #<n>`, `Ref … #<n>`, and any
   `OQ-n` in #17 whose home or text references the changed decision. (`gh issue
   list` + `gh issue view`, or `gh search`.)
3. For each dependent, judge whether the change invalidates an assumption it
   made. Produce a **change-impact report**: dependent issue → what it assumed →
   whether it's now stale → suggested action.
4. **Report first.** Apply edits (move items back to Open, add a "revisit"
   note) only after the user confirms. Don't silently rewrite dependents.

This is the holistic guard: a small mechanic rarely changes alone.

## Notes

- Prefer `--body-file <tmp>` for body rewrites (avoids shell-escaping markdown).
  `gh issue comment` is fine for an append-only decision trail if preferred.
- Don't invent rationale. If a decision's "why" isn't clear from the
  conversation, ask before recording it.
