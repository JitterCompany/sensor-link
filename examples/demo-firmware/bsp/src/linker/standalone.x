/*
Linker script for standalone (non-bootload) firmware.
Such firmware will overwrite the bootloader + application (but not the config)
*/
INCLUDE inc_base.x

MEMORY
{
  FLASH : ORIGIN = ORIGIN(FLASH_AVAILABLE), LENGTH = LENGTH(FLASH_AVAILABLE)
  RAM : ORIGIN = ORIGIN(RAM_AVAILABLE), LENGTH = LENGTH(RAM_AVAILABLE)
}

