# Website next steps

Recorded from Manny's direction on 29 September 2026. These are queued work
items, not implemented features or completed measurements.

## 1. Your first PS1 game

Create a beginner platformer tutorial for the website, inspired by the teaching
approach of Brackeys' Godot tutorial: start from nothing and finish with a small,
playable game. Go beyond the existing hello-tri walkthrough.

- Review the reference tutorial and map its learning progression to PSoXide.
- Choose and explain the editor/engine or SDK route after checking which offers
  the clearest, reproducible beginner experience.
- Suggested milestones: a level and player, movement and jumping, collision,
  camera, animation, collectibles, hazards, sound, and restarting or finishing.
- Give each milestone runnable source, an explanation, and a playable result.
- Use suitable original or openly licensed assets with credits.
- Finish with a downloadable project and instructions to build a PS1 disc and
  run it in the emulator or on original hardware.

Done when a reader can follow the guide from a fresh checkout to a complete
small platformer, with each published checkpoint verified against its source.

## 2. Embed the emulator throughout the site

Implemented on 30 September 2026: six SDK examples, an examples gallery, five
focused how-tos, and players in the first-program guide and emulator page.
Sources are pinned, builds include hashes, players load on demand and only one
instance remains started per page. The first-game tutorial checkpoints and
touch controls are still future work.

Let visitors play the outcome of the code directly beside the explanation.
Use the tutorial checkpoints first, then selected SDK examples and showcases.

- Build one reusable embedded-player component that loads a specified example.
- Investigate direct hosting of the web emulator on GitHub Pages, including
  WASM/assets, subpath handling and any required browser headers.
- Keep the emulator build and example artifacts pinned together so the displayed
  code and playable result agree.
- Provide clear play, pause, restart, audio, focus and input controls; check
  keyboard/gamepad behaviour and establish what works on mobile.
- Load players on demand and pause inactive instances so several embeds do not
  consume CPU, memory or audio simultaneously.
- Give loading/error states a useful fallback and retain standalone play links.
- Prefer small, self-contained tutorial builds over loading the full demo disc
  for every code example.

Done when several examples can coexist on one page and visitors can reliably
play the corresponding code without leaving the site.

## 3. Measure the missing results

Collect evidence rather than leaving measurement gaps as permanent placeholders.

- Inventory the missing accuracy results in `data/accuracy.toml` and identify
  which existing speed, CPU and memory measurements need refreshing.
- Select public test suites with explicit pass/fail criteria and run them on
  pinned builds of PSoXide and the comparison emulators.
- Reuse documented original-console captures where applicable; identify new
  hardware runs needed and keep emulator-only evidence distinct.
- Record failures, skipped tests and crashes as well as passes. Save raw logs,
  versions, configuration, run dates and reproducible commands with the results.
- Refresh performance measurements under comparable conditions, with repeated
  runs and separate headless versus interactive/browser costs.
- Generate site data and explanations from those artifacts, including limits
  and variability. Do not infer accuracy from boot compatibility or frame rate.
- Collect results regardless of whether PSoXide leads. The existing speed-chart
  visibility setting is separate from doing the measurement work.

Done when the site can replace missing results with reproducible measurements
and links to their evidence.

## Suggested order

Prototype one small embedded SDK example to validate delivery, then build the
platformer tutorial around runnable checkpoints. The measurement work is
independent and can proceed alongside those tasks.
