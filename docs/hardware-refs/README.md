# Hardware references

This directory is the one home of the hardware-test captures and notes: the `hwtest-machine-code-*`
baselines, the `px7-*`/`px8-*` emulator and silicon captures and the per-subsystem notes
(`gpu.md`, `dma.md`, `irq.md`, ...). The SDK owns them because the hardware-test program
(`engine/examples/hardware-tests`), its disc build (`tools/engine-examples.mk`) and the readers of the
captures (`tools/psoxide-hwtest`) live here.

PSoXide-editor carried an identical copy (checked on 2026-11-08: 76 files, no difference). It should
drop that copy and read these files through its hydrated SDK instead; its `HWTEST_BASELINE` in the
Makefile and `docs/README.md` links are the only references. New captures are added here.
