/* A program on the scene core: the stack starts below the core's window. */
RAM_SIZE_OVERRIDE = 0x1FE000;
/* Where its overlays run (psoxide-overlay.ld): memory it would otherwise use as heap. */
OVERLAY_VMA = 0x80100000;
