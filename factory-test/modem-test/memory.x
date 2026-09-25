/*
Standalone image for the STM32L4R7: overwrites everything in flash except the
config sector at the very end.
*/
MEMORY
{
  FLASH_ALL : ORIGIN = 0x08000000, LENGTH = 2M
  RAM_ALL : ORIGIN = 0x20000000, LENGTH = 640K

  /* Config flash (certificates etc): 1x8K.
     Kept out of FLASH so flashing this image leaves a device's config intact.
  */
  FLASH_CFG : ORIGIN = ORIGIN(FLASH_ALL) + LENGTH(FLASH_ALL) - 8K, LENGTH = 8K
  FLASH_AVAILABLE : ORIGIN = ORIGIN(FLASH_ALL), LENGTH = LENGTH(FLASH_ALL) - LENGTH(FLASH_CFG)

  /* Pinned RTT control block region: 256 B at the very end of RAM (0x2009FF00),
     just enough for the SEGGER RTT control block (header + a few channel
     descriptors). Capture tools attach at ORIGIN(RAM_RTT) via `-RTTAddress`.
     Channel buffers live in normal .bss. */
  RAM_RTT : ORIGIN = ORIGIN(RAM_ALL) + LENGTH(RAM_ALL) - 256, LENGTH = 256

  /* Define how much of the RAM is used for stack.
     If set too high it won't compile (globals won't fit in the remainder)
  */
  RAM_STACK : ORIGIN = ORIGIN(RAM_ALL), LENGTH = 500K
  RAM_AVAILABLE : ORIGIN = ORIGIN(RAM_STACK) + LENGTH(RAM_STACK), LENGTH = LENGTH(RAM_ALL) - LENGTH(RAM_STACK) - LENGTH(RAM_RTT)

  FLASH : ORIGIN = ORIGIN(FLASH_AVAILABLE), LENGTH = LENGTH(FLASH_AVAILABLE)
  RAM : ORIGIN = ORIGIN(RAM_AVAILABLE), LENGTH = LENGTH(RAM_AVAILABLE)
}

/*
Put stack in dedicated section.
This is important for memory safety, as it prevents a stack overflow from going undetected
and silently overwriting global variables  (bss/data/noinit sections).
As the stack grows 'backwards' to ORIGIN(RAM_STACK), an overflow will
trigger a hardfault because the address below that (0x1FFF_FFFF) is reserved (end of RAM).
This is the same trick that 'flip-link' does.
*/
_stack_start = ORIGIN(RAM_STACK) + LENGTH(RAM_STACK);

SECTIONS {
    /* RTT control block, pinned at ORIGIN(RAM_RTT).
       NOLOAD because rtt_init! initialises at runtime. */
    .rtt_cb (NOLOAD) : ALIGN(4)
    {
        KEEP(*(.rtt_cb))
    } > RAM_RTT
}
