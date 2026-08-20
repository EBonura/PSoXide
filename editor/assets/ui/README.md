# Default 2D UI assets

`health_bar_clean_slim.png` is the canonical empty-to-full health-gauge strip:

- 106 x 203 pixels
- seven vertical frames, each 106 x 29 pixels
- frame 0 is empty and frame 6 is completely full
- transparent background

`health_bar_clean_slim.psxt` is cooked from the complete strip in one pass so
all frames share one 16-entry CLUT. Rebuild it with:

```sh
cargo run -p psxed-tex --bin psxt-convert -- \
  editor/assets/ui/health_bar_clean_slim.png \
  editor/assets/ui/health_bar_clean_slim.psxt \
  106 203 4 --transparent-zero
```

Starter projects keep a byte-identical project-local PSXT copy under
`assets/ui/`, preserving the normal self-contained project/cook contract.
