# Frontend Font Provenance

## VT323-Regular.ttf

- **Designer**: Peter Hull
- **Source**: https://fonts.google.com/specimen/VT323
- **License**: SIL Open Font License 1.1
  (https://openfontlicense.org/) - full text embedded in the TTF's
  metadata.
- **Used for**: monospace / debugger / register-panel typography in
  the PSoXide frontend.

## Phosphor.ttf / Phosphor-Fill.ttf

- **Source**: https://phosphoricons.com/
  (`@phosphor-icons/web`, regular + fill weights).
- **License**: MIT
  (https://github.com/phosphor-icons/homepage/blob/master/LICENSE).
- **Used for**: UI iconography in the frontend (codepoints listed in
  `emu/crates/frontend/src/icons.rs`). Regular is the default weight;
  fill is used for active toggle buttons.

All fonts are bundled as binary `.ttf` files. Their licenses are
GPL-compatible: the SIL Open Font License is explicitly compatible
with GPL when fonts are bundled with software, and MIT is a
permissive license.
