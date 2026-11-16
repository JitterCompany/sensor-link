# Linker scripts

## Flash memory map

The internal flash memory is generally split across three main sections:

### 1. Bootloader
   Code that runs when the CPU is reset / powered on.
   Last 128 bytes: bootloader metadata (version)

### 2. Application
First 512 bytes: metadata used by the bootloader (length, checksum, ..)
Rest of the section: Application code

### 3. Config
Stores config data that is provisioned into the device once (serial number, certificates, ..).
Stored at the end of the flash memory so it should survive reflashing even standalone code

**Note:** _Standalone firmware has no concept of bootloader/application. Instead it has one merged application section but it still preserves the config._
