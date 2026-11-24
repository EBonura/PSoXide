/* hello-boot's negative boot-stack build (build.sh): a boot-time stack smaller than the splash's
   own need. The link must refuse it. */
BOOT_STACK_OVERRIDE = 0x1000;
