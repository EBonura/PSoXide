# Hardware references

This directory is the one home of the hardware-test captures and notes: the `hwtest-machine-code-*`
baselines, the `px7-*`/`px8-*` emulator and silicon captures and the per-subsystem notes
(`gpu.md`, `dma.md`, `irq.md`, ...). The SDK owns them because the hardware-test program
(`engine/examples/hardware-tests`), its disc build (`tools/engine-examples.mk`) and the readers of the
captures (`tools/psoxide-hwtest`) live here.

PSoXide-editor carried an identical copy (checked on 2026-11-08: 76 files, no difference). It should
drop that copy and read these files through its hydrated SDK instead; its `HWTEST_BASELINE` in the
Makefile and `docs/README.md` links are the only references. New captures are added here.

The console references `psoxide-hwtest compare` reads, oldest first (a later file wins where two carry the same
record): `px8-silicon-2026-09-23-v1.24-full.txt`, `px8-silicon-2026-10-08-v1.28-cdstream.txt`,
`px8-silicon-2026-11-14-v2.18-run1-scph1200.txt` and `px8-silicon-2026-11-14-v2.18-run2-scph110.txt`. The last two
are the QR pages of two runs of hardware-tests v2.18 on a console (SCPH-9002) on 2026-11-14, one with an SCPH-1200 and
one with an SCPH-110, both DualShocks. Run 2 had its button prompts answered, which is why it carries 23 records
(`0x940`-`0x956`) that run 1 does not.
