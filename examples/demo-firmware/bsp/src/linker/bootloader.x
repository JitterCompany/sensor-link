/*
Linker script for the bootloader

Shares the inc_bootload.x memory map with the application.
the alias FLASH is used (by the cortex-m crate) to determine where to put
the code.
*/
INCLUDE inc_bootload.x

MEMORY
{
  FLASH : ORIGIN = ORIGIN(FLASH_BOOTLOADER), LENGTH = LENGTH(FLASH_BOOTLOADER)
}

SECTIONS {
    .flash_bootloader_meta :
    {
        KEEP(*(.flash_bootloader_meta*))
    } > FLASH_BOOTLOADER_META
}