---
name: playtest-log
description: Capture structured playtest/feel observations for Kill the Serpent — especially the Milestone-1 feel spikes. Use after running the ROM or a spike build (see preview-rom / run) to record what felt good vs wrong against the spike's "Done = feels like X" bar, tie findings to the open-questions register (OQ-n in #17), and hand any resulting decision to design-sync. Encodes the prototype → playtest → refine loop from docs/design/PILLARS.md.
---

# playtest-log

The prototype-first discipline only works if feel observations are *captured*,
not lost. This skill standardizes that capture and wires it to the design record.

## When to use
- After a `preview-rom` / `run` of a spike or feature, or any hands-on feel
  session. Especially the M1 spikes (#18–20), whose whole point is a feel verdict.

## Inputs to gather
- **Which issue/spike** is being tested, and its **"Done = feels like X"** bar.
- The **build**: commit SHA + profile (debug/release).
- **Input method**: handedness setting, stylus vs. emulator mouse, which controls.
- The relevant **OQ-n** from #17 this session informs (e.g. Spike A → OQ-1).

## Steps
1. **Anchor.** Read the target issue's "Done =" criteria and the matching OQ-n in
   #17. State the bar before judging against it.
2. **Capture** (use `AskUserQuestion` for structured prompts if interactive, or
   record from direct observation) into this shape:

   ```
   # Playtest — <issue> — <YYYY-MM-DD>
   Build: <sha> (<profile>)   Input: <handedness/stylus>
   Tested: <what was exercised>
   Felt good: <…>
   Friction / felt wrong: <… — this is the valuable part, be honest>
   Against the bar ("Done = …"): met / not met / partial — <why>
   Informs: OQ-<n> — <what it tells us>
   Verdict: PROCEED | PIVOT | KILL — <next action>
   ```
3. **Record on the board.** Post the log as a **comment on the spike/issue**
   (`gh issue comment <n>`) so the feel trail lives with the design record, not in
   chat.
4. **Hand off decisions.** If the session resolves or sharpens an open question,
   or yields a design decision, run **`design-sync`** to write it to the owning
   issue (Locked + rationale) and update #17's register. A playtest that changes
   the design but doesn't reach an issue didn't really happen.

## Notes
- Absolute dates. Friction is the point — don't sand off the bad parts.
- One session → one verdict. "Felt okay" is not a verdict; PROCEED/PIVOT/KILL is.
