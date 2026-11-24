/* hello-boot's boot-in-the-reserve build (build.sh): 0x5800 bytes of stack while boot code runs
   (the splash's 0x5000 and main's frames), and .boot run from 0x9000 below that, so its end lies
   inside the 32 KiB stack reserve, as Ocarina's does (it sets only BOOT_STACK_OVERRIDE: its .bss
   ends near the reserve already). */
BOOT_STACK_OVERRIDE = 0x5800;
BOOT_VMA_OVERRIDE = STACK_INIT - 0x5800 - 0x9000;
