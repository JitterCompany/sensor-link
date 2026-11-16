//! Bootloader
//!
//! This is the first code that runs when the microcontroller boots.
//! The bootloader:
//! 1. checks the SPI flash for firmware updates
//! 2. upgrades the application firmware if a valid update was found
//! 3. runs the application firmware if it is valid
#![no_std]
#![no_main]

use core::{fmt::Write as _, mem::MaybeUninit, panic::PanicInfo};
use rtic::app;
use rtic_sync::{
    channel::{Receiver, Sender},
    make_channel,
};

use demo_firmware_core::{
    bootloader::{
        apply_update, common_rtt_logger, common_time::FastClock, validate_update, AppStatus,
    },
    drivers::{spi_flash, time},
    log,
    meta::{
        bootloader::BootloaderMeta,
        firmware::{ApplicationHeader as FirmwareHeader, SecurityLevel, ValidationError},
    },
    monotonic_time::{delay_ms, delay_us},
    storage::flash_db::FileStore,
};
use hardware::{cortex_m, mono::Mono};

mod meta_section;
mod pubkey;

fn print_boot_message(meta: &BootloaderMeta) {
    let version = meta.version;
    let git_rev = meta.git_revision;
    log::info!("******");
    log::info!("# Starting Demo Bootloader v{version} ({git_rev})");
    log::info!("******\n");
}

use cortex_m_rt::{exception, ExceptionFrame};

#[exception]
unsafe fn HardFault(ef: &ExceptionFrame) -> ! {
    panic!("HardFault: {:#?}", ef);
}

#[exception]
unsafe fn DefaultHandler(irqn: i16) {
    panic!("DefaultHandler: unexpected irq {irqn}");
}

/// Print and reboot: a bootloader that stops would leave the board dead
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    critical_section::with(|_| {
        // Safety: channel 0 is the one `init` created, and the reboot that
        // follows makes any race with the logger moot.
        if let Some(mut channel) = unsafe { rtt_target::UpChannel::conjure(0) } {
            channel.set_mode(rtt_target::ChannelMode::NoBlockTrim);
            writeln!(channel, "{info}").ok();
        }
        hardware::reboot()
    })
}

use demo_firmware_core::storage::{
    files::File,
    flash_db::{block_layer::BlockDevice, Database},
    streams::{Stream, BLOCK_SIZE},
};

use crate::pubkey::app_pubkey;

type SPIFlash = spi_flash::SPIFlash<hardware::SPIDevice, 4096>;
type PersistentStore = Database<BlockDevice<SPIFlash, BLOCK_SIZE>, BLOCK_SIZE, File, Stream>;

const FW_FILE: File = File::Firmware;
const CHUNK_SIZE: usize = FW_FILE.max_fragment_size();

#[app(device=hardware::rtic_device, peripherals = true, dispatchers = [FLASH, SPI2])]
mod app {

    use super::*;

    #[shared]
    struct Shared {}

    #[local]
    struct Local {
        led: hardware::Pin,
        internal_flash: hardware::internal_flash::InternalFlash,
        firmware_store: PersistentStore,
    }

    #[derive(Debug, Clone, Copy)]
    enum BlinkPattern {
        /// Bootloader 'idle'
        Wait,

        /// Update busy (writing internal flash)
        Updating,

        Error,

        BootApplication,
    }

    #[init(local=[
        fastclock: MaybeUninit<FastClock> = MaybeUninit::uninit(),
    ])]
    fn init(cx: init::Context) -> (Shared, Local) {
        // Not in `.rtt_cb`: provisioning attaches at the app's pinned address
        // for its UID print, and finds a zero (NOLOAD) control block there
        // until the app initialises it. A separate session without
        // `-RTTAddress` still captures this log during development.
        let channels = rtt_target::rtt_init! {
            up: {
                0: {
                    size: 2048,
                    mode: rtt_target::ChannelMode::NoBlockTrim,
                    name: "Print Output",
                }
            }
        };
        common_rtt_logger::init_lock_logger(channels.up.0);

        let _cortex: cortex_m::Peripherals = cx.core;

        let hw = hardware::board_init_minimal(cx.device);

        let metadata = hw.internal_flash.bootloader_metadata().unwrap();
        print_boot_message(&metadata);

        time::init_timer(cx.local.fastclock.write(FastClock::new())).ok();

        let (blink_s, blink_r) = make_channel!(BlinkPattern, 1);
        blink_task::spawn(blink_r).ok();
        bootload_task::spawn(blink_s).unwrap();

        let spi_flash = {
            let descriptor = hardware::SPI_FLASH_DESCRIPTOR
                .validate()
                .expect("Misconfigured SPI flash");
            spi_flash::SPIFlash::new(hw.spi_flash, descriptor)
        };

        let blockdev = BlockDevice::readonly_from(spi_flash);
        let firmware_store = Database::new(blockdev);

        log::info!("Bootloader: Initialized");

        (
            Shared {},
            Local {
                led: hw.led,
                internal_flash: hw.internal_flash,
                firmware_store,
            },
        )
    }

    #[task(priority = 0)]
    async fn boot(_cx: boot::Context) {
        unsafe {
            hardware::boot_application();
        }
    }

    #[task(priority = 3, local = [led], shared = [])]
    async fn blink_task(cx: blink_task::Context, mut receiver: Receiver<'static, BlinkPattern, 1>) {
        let mut set_led = |on: bool| {
            if on {
                cx.local.led.set_high();
            } else {
                cx.local.led.set_low();
            }
        };

        // Default pattern: is shown at least once.

        // Note that the duration of the pattern as a side-effect
        // defines the minimum time the bootloader is active.
        // This can be used as 'debrick delay' during which
        // a debugger can be attached even if the main firmware is corrupt/buggy
        let mut pattern = BlinkPattern::Wait;

        loop {
            // Show the blink pattern (one sequence)
            match pattern {
                BlinkPattern::Wait => {
                    for _ in 0..4 {
                        set_led(true);
                        delay_us(80_000).await;
                        set_led(false);
                        delay_ms(80).await;
                    }
                }
                BlinkPattern::Updating => {
                    // Short blink (effective timing may vary due to critical sections)
                    set_led(true);
                    delay_ms(20).await;
                    set_led(false);
                    delay_ms(100).await;
                }
                BlinkPattern::Error => {
                    // Triple strobe + long pause
                    for _ in 0..3 {
                        set_led(true);
                        delay_ms(50).await;
                        set_led(false);
                        delay_ms(50).await;
                    }
                    delay_ms(700).await;
                }
                BlinkPattern::BootApplication => {
                    set_led(true);

                    // Note: even though not necessary for the blink patter,
                    // some delay *must* be set to give lower prio tasks a chance
                    delay_ms(500).await;
                }
            }

            // Apply new pattern (if any)
            if let Ok(new_pattern) = receiver.try_recv() {
                log::debug!("blink {pattern:?}");
                pattern = new_pattern;
            }
        }
    }

    #[task(priority = 2, local = [internal_flash, firmware_store], shared=[])]
    async fn bootload_task(
        cx: bootload_task::Context,
        mut blinker: Sender<'static, BlinkPattern, 1>,
    ) {
        let internal_flash = cx.local.internal_flash;

        let firmware_store = cx.local.firmware_store;

        let Ok(config) = internal_flash.config() else {
            log::error!("Refusing to boot: no provisioned config!");
            log::warn!("Device may be bricked untill manual reflashing.");
            log::warn!("Will retry in 30 minutes...");
            blinker.send(BlinkPattern::Error).await.ok();
            delay_ms(30 * 60_000).await;
            hardware::reboot();
        };

        let device_type = config.device_type;
        // Store exists? Try to find new firmware
        let init_res = firmware_store.initialize().await;
        log::debug!("Firmare store init: {:?}", init_res);
        let app_pubkey = app_pubkey();
        let application = match validate_update::<FirmwareHeader, _, _, _, BLOCK_SIZE, CHUNK_SIZE>(
            internal_flash,
            firmware_store,
            FW_FILE,
            device_type,
            &app_pubkey,
            // Only a signed image may boot, from flash or from an update
            SecurityLevel::Signed,
        )
        .await
        {
            AppStatus::UpToDate(header) => {
                log::info!("Skipping update: already up-to-date!");
                header
            }
            AppStatus::UpdateRejected(header, error) => {
                match error {
                    ValidationError::Incomplete => {
                        log::warn!("Skipping update: no update available (or incomplete)");
                    }
                    _ => {
                        log::warn!("Skipping update: validation failed ({error:?})");
                        blinker.send(BlinkPattern::Error).await.ok();
                        delay_ms(2_000).await;
                    }
                }

                header
            }
            AppStatus::ShouldUpdate(new_header) => {
                log::info!(
                    "Updating to {:?}-byte {:?}-protected image with minimum version {}",
                    new_header.length(),
                    new_header.security_level(),
                    new_header.anti_downgrade_version()
                );
                blinker.send(BlinkPattern::Updating).await.ok();
                match apply_update::<FirmwareHeader, _, _, _, BLOCK_SIZE, CHUNK_SIZE>(
                    internal_flash,
                    firmware_store,
                    FW_FILE,
                    device_type,
                    &app_pubkey,
                )
                .await
                {
                    Ok(header) => {
                        log::info!("Success: Update applied!");
                        header
                    }
                    Err(error) => {
                        log::error!("Update failed: {error:?}");
                        log::warn!("Will retry in 15 minutes...");
                        blinker.send(BlinkPattern::Error).await.ok();
                        delay_ms(15 * 60_000).await;
                        hardware::reboot();
                    }
                }
            }
            AppStatus::Bricked(old, new) => {
                // Should never happen in production. The retry is a last resort to recover
                // in case there actually is a valid firmware but it failed due to a glitch in hardware.
                // The retry is slow on purpose to limit flash wear: 30 minutes * 10k cycles = 200 days before
                // the flash starts to wear out..
                log::error!(
                    "Refusing to boot: invalid application ({old:?}) and no update ({new:?})!"
                );
                log::warn!("Device may be bricked untill manual reflashing.");
                log::warn!("Will retry in 30 minutes...");
                blinker.send(BlinkPattern::Error).await.ok();
                delay_ms(30 * 60_000).await;
                hardware::reboot();
            }
        };

        if let Some(len) = application.length() {
            log::debug!(
                "Booting {len}-byte application (security level {:?}, downgrade level {:?})",
                application.security_level(),
                application.anti_downgrade_version(),
            );
        } else {
            log::debug!(
                "Booting unknown application with security level {:?}",
                application.security_level()
            );
        }

        // Boot via separate task (must be done from 'idle' context (prio 0).)
        // By awaiting two blink patters, we know that at least the first must have taken
        // effect (queue size is 1)
        blinker.send(BlinkPattern::BootApplication).await.ok();
        blinker.send(BlinkPattern::BootApplication).await.ok();
        boot::spawn().unwrap();

        // If app is somehow not booted, reboot and try again
        delay_ms(3_000).await;
        log::warn!("Failed to boot app? Rebooting...");
        delay_ms(1_000).await;
        hardware::reboot();
    }

    #[task(binds=TIM5, priority=7)]
    fn tim5(_cx: tim5::Context) {
        unsafe {
            Mono::interrupt_handler();
        }
    }

    #[task(binds=SPI1, priority=5)]
    fn spi_isr(_cx: spi_isr::Context) {
        unsafe {
            hardware::SPI::isr_bus1();
        }
    }
}
