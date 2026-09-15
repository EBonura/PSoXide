# Firmware cleanup, 2026-09-15

## Repository ownership

- `EBonura/PSoXide`: SDK, shared formats and disc mastering.
- `EBonura/PSoXide-emulator`: CPU, peripherals, settings and standalone player.
- `EBonura/PSoXide-editor`: editor, engine, Cortex and integrated player.

The local pre-split checkout was 24 commits behind the SDK main branch.
The `bios-independence` branch is already an ancestor of main; there is no
open firmware-cleanup PR in any of the three repositories at audit time.
Earlier provenance work is recorded in SDK commits `321496ff` and `6c7f5d57`.

## Changes

External firmware selection, environment loading, browser uploads, persisted
paths, command-line overrides and firmware warmup are removed. Every supported
EXE/disc launch uses the independently implemented runtime. Bad disc images
return a boot error. Games requiring external firmware are unsupported.
The SDK's debug TTY call and hardware register/interface descriptions remain:
these describe compatibility with original hardware and contain no ROM image.
The `mipsel-sony-psx` target name is the compiler's platform identifier.
Attribution and trademark notices remain intact.

Twenty bundled homebrew EXEs in each player repository had stale vendor text
in the optional header area at 0x4c. That area is now zero-filled; executable
header fields before 0x4c remain unchanged. Nineteen payloads are byte-identical.
The memory-card example also replaces its old vendor-BIOS prompt with the
same-length "CHECK CARD MENU" text; all other payload bytes remain unchanged. The SDK
linker discards legacy `.region` input sections so old object files cannot
reintroduce a vendor marker.

Legacy probes whose purpose was to execute external firmware were retired.
The SDK fixture harness and renderer EXE tools now use the built-in runtime.
Editor helper tools no longer select or pass firmware to external emulators.

## Audit scope and limits

The initial scan covered fetched remote history in all three repositories:
49,990 objects in the SDK clone, 10,086 in the emulator and 31,498 in the editor.
No 512 KiB blobs or suspicious firmware/SDK artifact paths were found. A separate
scan of tracked binary contents found the bundled EXE header strings above;
filename checks alone did not detect them.

Run `python3 tools/sony-material-audit.py` for the current working files, and
add `--history` for fetched remote refs and tags. Use `--repo PATH` to audit a
sibling checkout. Fetch remote refs and tags before interpreting history results.
The check fails on suspicious artifact paths, 512 KiB blobs or vendor text in
homebrew EXE headers. CI runs the working-file audit and its regression fixtures.
A finding requires inspection; matching a size or filename does not establish
ownership. This is not a semantic source-authorship proof.

Historical commits still contain earlier BIOS-loading implementations and old
homebrew headers. This cleanup changes current source and assets; it does not
rewrite history or invalidate downstream pinned commits. Release attachments,
external websites, local backup bundles and other repositories are outside this
source-tree audit. The old checkout's local archive is recovery material, not a
release input.
