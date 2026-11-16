extern "C" {
    pub static __FLASH_TOTAL_START: u32;
    pub static __FLASH_TOTAL_LENGTH: u32;

    pub static __FLASH_BOOTLOADER_START: u32;
    pub static __FLASH_BOOTLOADER_LENGTH: u32;

    pub static __FLASH_BOOTLOADER_META_START: u32;
    pub static __FLASH_BOOTLOADER_META_LENGTH: u32;

    pub static __FLASH_CFG_START: u32;
    pub static __FLASH_CFG_LENGTH: u32;

    pub static __FLASH_APP_META_START: u32;
    pub static __FLASH_APP_META_LENGTH: u32;

    pub static __FLASH_APP_CODE_START: u32;
    pub static __FLASH_APP_CODE_LENGTH: u32;

    pub static __FLASH_APP_TOTAL_START: u32;
    pub static __FLASH_APP_TOTAL_LENGTH: u32;

    pub static __RAM_STACK_START: u32;
    pub static __RAM_STACK_LENGTH: u32;
}

/// Get a value from the linker script
pub unsafe fn symbol(linker_symbol: &u32) -> u32 {
    let value_as_ptr: *const u32 = linker_symbol;
    value_as_ptr as u32
}
