/*
Linker script for a bootloadable application

Shares the inc_bootload.x memory map with the bootloader.
the alias FLASH is used (by the cortex-m crate) to determine where to put
the code.
*/
INCLUDE inc_bootload.x

MEMORY
{
  FLASH : ORIGIN = ORIGIN(FLASH_APP_CODE), LENGTH = LENGTH(FLASH_APP_CODE)
}

SECTIONS {
    .flash_app_meta :
    {
        KEEP(*(.flash_app_meta*))
    } > FLASH_APP_META
}
