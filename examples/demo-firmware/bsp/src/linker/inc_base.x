MEMORY
{
  /* -- Private (CPU-specific) memory areas -- */
  FLASH_ALL : ORIGIN = 0x08000000, LENGTH = 2M
  RAM_ALL : ORIGIN = 0x20000000, LENGTH = 640K


  /* -- Sections below can be used by other linker scripts -- */

  /* Config flash (certificates etc): 1x8K
     By putting it all the way at the end it is not likely to be overwritten by other firmware
  */
  FLASH_CFG : ORIGIN = ORIGIN(FLASH_ALL) + LENGTH(FLASH_ALL) - 8K, LENGTH = 8K
  FLASH_AVAILABLE : ORIGIN = ORIGIN(FLASH_ALL), LENGTH = LENGTH(FLASH_ALL) - LENGTH(FLASH_CFG)

  /* Pinned RTT control block region: 256 B at the very end of RAM,
     just enough for the SEGGER RTT control block (header + a few
     channel descriptors). Provisioning's JLinkRTTLogger attaches at
     ORIGIN(RAM_RTT) via `-RTTAddress`. Channel BUFFERS live in normal
     .bss so their size can depend per application.
     The bootloader's CB lives in its own .bss (deliberately not shared). */
  RAM_RTT : ORIGIN = ORIGIN(RAM_ALL) + LENGTH(RAM_ALL) - 256, LENGTH = 256

  /* Define how much of the RAM is used for stack.
     If set too high it won't compile (globals won't fit in the remainder)
  */
  RAM_STACK : ORIGIN = ORIGIN(RAM_ALL), LENGTH = 500K
  RAM_AVAILABLE : ORIGIN = ORIGIN(RAM_STACK) + LENGTH(RAM_STACK), LENGTH = LENGTH(RAM_ALL) - LENGTH(RAM_STACK) - LENGTH(RAM_RTT)
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
_stack_end = ORIGIN(RAM_STACK);

/*
Global regions: valid for all firmware variants
*/
__FLASH_TOTAL_START = ORIGIN(FLASH_ALL);
__FLASH_TOTAL_LENGTH = LENGTH(FLASH_ALL);

__FLASH_CFG_START = ORIGIN(FLASH_CFG);
__FLASH_CFG_LENGTH = LENGTH(FLASH_CFG);

__RAM_STACK_START = ORIGIN(RAM_STACK);
__RAM_STACK_LENGTH = LENGTH(RAM_STACK);

__RAM_RTT_START = ORIGIN(RAM_RTT);
__RAM_RTT_LENGTH = LENGTH(RAM_RTT);

/*
Weak regions: default to empty, may be overriden
by specific linker script (e.g. inc_bootload.x)
*/
PROVIDE(__FLASH_BOOTLOADER_START = 0);
PROVIDE(__FLASH_BOOTLOADER_LENGTH = 0);

PROVIDE(__FLASH_BOOTLOADER_META_START = 0);
PROVIDE(__FLASH_BOOTLOADER_META_LENGTH = 0);

PROVIDE(__FLASH_APP_META_START = 0);
PROVIDE(__FLASH_APP_META_LENGTH = 0);

PROVIDE(__FLASH_APP_CODE_START = 0);
PROVIDE(__FLASH_APP_CODE_LENGTH = 0);

PROVIDE(__FLASH_APP_TOTAL_START = 0);
PROVIDE(__FLASH_APP_TOTAL_LENGTH = 0);


SECTIONS {
    .flash_config :
    {
        KEEP(*(.flash_config*))
    } > FLASH_CFG

    /* App's RTT control block, pinned at ORIGIN(RAM_RTT). Provisioning
       tooling passes this address to JLinkRTTLogger via -RTTAddress.
       NOLOAD because rtt_init! initialises at runtime. */
    .rtt_cb (NOLOAD) : ALIGN(4)
    {
        KEEP(*(.rtt_cb))
    } > RAM_RTT
}
