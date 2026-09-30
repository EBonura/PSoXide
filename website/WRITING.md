# PSoXide website writing guide

Reviewed 29 September 2026. Use clear, conversational technical prose: explain
what the software does, what a visitor can do with it, and where its limits are.

## Research

- [Godot](https://godotengine.org/) identifies its product category and open-source
  status first, then describes supported uses and links to downloads and docs.
- [Bevy](https://bevy.org/) identifies the engine and its implementation language,
  then explains concrete features with examples. It also uses promotional
  adjectives; those are not the part we are adopting.
- [PCSX-Redux](https://pcsx-redux.consoledev.net/) organizes its documentation
  around tasks such as installing, compiling and debugging. This is a relevant
  example for an emulator and PlayStation development audience.
- [Google's developer writing guidance](https://developers.google.com/style/tone)
  recommends friendly, direct language that prioritizes useful information. It
  discourages hype, figurative language, unnecessary jargon and assurances that
  a procedure is easy.

Our interpretation: lead with purpose and supported tasks, explain capabilities
with examples, and let screenshots, working demos and measurements demonstrate
what the project can do. Technical websites vary in tone; these are selected
practices, not a claim that all technical sites use the same voice.

## Editorial rules

1. Say what a tool does before describing its implementation. Introduce hardware
   abbreviations when the reader needs them, rather than listing them in a hero.
2. Use concrete headings: “Game ports”, “Headless memory”, “Build a disc image”.
   Avoid “impossible”, “revolutionary”, “ultimate” and unsupported superlatives.
3. Describe measured results with their build, workload and conditions. Keep
   caveats close to the figures; a headless memory result is not desktop usage.
4. Distinguish released features, experiments and plans. Do not promise that a
   future optimization will improve a result or imply unfinished work is ready.
5. Give instructions as actions, without “just”, “simply” or “one easy command”.
6. Keep game descriptions readable. Explain the mechanics and release status;
   link to implementation detail for readers who want it.
7. Keep credits and distribution requirements precise. A tone edit must not
   change licensing terms, attribution or the meaning of compatibility tiers.
8. Preserve the actual titles of linked videos and other external works.

## Examples from the revision

| Before | After |
| --- | --- |
| Making impossible PS1 ports possible. | Development tools for the original PlayStation. |
| Ports that shouldn't fit | Game ports |
| Memory: leads | Headless memory |
| …so it never runs out. | …loaded in chunks as you explore. |

Check the rendered page after editing. A factual sentence still needs to fit the
layout, and a shorter label must still make its destination clear.

## Project framing

Lead with “What if we never stopped developing for the original PlayStation?”
and explain three strands: **Create. Port. Optimize.** Create means new games;
Port means games never released on PS1; Optimize means rewriting the original
source of existing PS1 games into more efficient implementations. Do not describe
the third strand as modding or claim efficiency gains without measurements.

Use “Modern tools for original hardware” and “Built for fast, testable iteration”
for the development approach. Explain coding-assistant interfaces in developer
material, with the existing AI-use and provenance disclosure on the About page.
Keep original-hardware results, emulator enhancements, plans and releases distinct.
