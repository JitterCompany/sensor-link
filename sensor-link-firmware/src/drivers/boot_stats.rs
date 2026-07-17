use strum::{EnumCount, IntoEnumIterator};

use crate::logic::signal::BootReason;

#[derive(Debug, Clone, Copy, num_enum::TryFromPrimitive, strum::EnumCount, strum::EnumIter)]
#[repr(usize)]
pub enum Stat {
    // Total uptime untill current boot
    UptimeTotal,

    // Total uptime during this boot (separate from total to minimize potential corruption)
    UptimeCurrent,
    Boot,
    POR,
    WDT,
    Panic,
    Fault,
}

const COUNT_MASK: u32 = 0x0FFF_FFFF;
const FLAG_MASK: u32 = !COUNT_MASK;
const FLAG: u32 = 0xB000_0000;

pub struct Stats {
    /// Reason for last (re-)boot
    pub boot_reason: BootReason,

    /// Total uptime of all previous bootcycles (since stats last reset)
    pub uptime_total: u32,

    /// Total amount of boot cycles during `uptime_total`
    pub boot_total: u32,

    /// Total amount of power-on cycles during `uptime_total`
    pub por_total: u32,

    /// Total amount of watchdog resets during `uptime_total`
    pub wdt_total: u32,

    /// Total amount of panic resets during `uptime_total`
    pub panic_total: u32,

    /// Total amount of fault resets during `uptime_total`
    pub fault_total: u32,
}

impl Default for Stats {
    fn default() -> Self {
        Self {
            boot_reason: BootReason::Unknown,
            uptime_total: 0,
            boot_total: 0,
            por_total: 0,
            wdt_total: 0,
            panic_total: 0,
            fault_total: 0,
        }
    }
}

fn read<U32: crate::traits::U32>(registers: &[U32], stat: Stat) -> u32 {
    registers[stat as usize].read()
}

fn write<U32: crate::traits::MutU32>(registers: &mut [U32], stat: Stat, value: u32) {
    registers[stat as usize].write(value)
}

fn read_split<U32: crate::traits::U32>(registers: &[U32], stat: Stat) -> (u32, bool) {
    let raw = read(registers, stat);
    (raw & COUNT_MASK, (raw & FLAG_MASK != 0))
}

/// Set flag on a register
///
/// This marks the register as the cause for reboot.
/// After reboot, Stats::try_from_registers() should reset the flag and increment the count
pub fn set_flag<U32: crate::traits::MutU32>(reg: &mut U32) {
    let prev = reg.read();
    reg.write((prev & COUNT_MASK) | FLAG);
}

/// Increment stats in a register (saturating)
pub fn increment_by<U32: crate::traits::MutU32>(reg: &mut U32, delta: u32) -> u32 {
    let prev = reg.read();
    let count = (prev & COUNT_MASK).saturating_add(delta).min(COUNT_MASK);
    reg.write(count | (prev & FLAG_MASK));
    count
}

impl Stats {
    /// Try to get stats from array of registers.
    ///
    /// Fails if the array is too short ( < Stat::COUNT )
    pub fn try_from_registers<U32: crate::traits::MutU32>(
        registers: &mut [U32],
    ) -> Result<Self, ()> {
        Self::try_from_registers_or(registers, BootReason::Unknown)
    }

    /// Try to get stats from array of registers.
    /// If no reason can be determined, fallback to given default
    ///
    /// Fails if the array is too short ( < Stat::COUNT )
    pub fn try_from_registers_or<U32: crate::traits::MutU32>(
        registers: &mut [U32],
        default: BootReason,
    ) -> Result<Self, ()> {
        if registers.len() < Stat::COUNT {
            return Err(());
        }

        // Validate stats: 'magic' FLAG marker mark the validity of the time stats
        let uptime_prev_flag = read(registers, Stat::UptimeCurrent) & FLAG_MASK;
        let uptime_total_flag = read(registers, Stat::UptimeTotal) & FLAG_MASK;

        let mut corrupt_fields = 0;
        if uptime_prev_flag != FLAG {
            corrupt_fields += 1;
            write(registers, Stat::UptimeCurrent, FLAG);
        }
        if uptime_total_flag != FLAG {
            corrupt_fields += 1;
            write(registers, Stat::UptimeTotal, FLAG);
        }

        // Stats cannot be trusted. Maybe the first time they are written or backup battery empty
        if corrupt_fields > 1 {
            Self::reset(registers)?;
        }

        // Update total uptime
        let uptime_prev = read(registers, Stat::UptimeCurrent) & COUNT_MASK;
        let uptime_total = increment_by(&mut registers[Stat::UptimeTotal as usize], uptime_prev);

        // clear current uptime. Application should increment this regularly
        write(registers, Stat::UptimeCurrent, FLAG);

        // Note: this may increment multiple stats at once (e.g. hardfault + panic if hardfaulthandler panicked)
        let boot_total = increment_by(&mut registers[Stat::Boot as usize], 1);

        let (mut por_total, por_flag) = read_split(registers, Stat::POR);
        let (mut wdt_total, wdt_flag) = read_split(registers, Stat::WDT);
        let (mut panic_total, panic_flag) = read_split(registers, Stat::Panic);
        let (mut fault_total, fault_flag) = read_split(registers, Stat::Fault);

        // Detect boot reason
        let mut boot_reason = match (fault_flag, panic_flag, wdt_flag, por_flag) {
            (true, _, _, _) => BootReason::HardFault,
            (false, true, _, _) => BootReason::Panic,
            (false, false, true, _) => BootReason::Watchdog,
            (false, false, false, true) => BootReason::PowerOn,
            (false, false, false, false) => default,
        };

        // Time interval passed: reset stats
        if uptime_total >= 1800 {
            Self::reset(registers)?;

        // Detect a boot loop if boot reason happens more than N times per interval
        } else {
            let loop_found = match boot_reason {
                // threshold should not too sensitive or it may trigger during dev / testing
                BootReason::PowerOn => {
                    por_total = por_total.saturating_add(1).min(COUNT_MASK);
                    write(registers, Stat::POR, por_total);

                    por_total > 20
                }
                BootReason::Software => boot_total > 30, // Note: no separate stat counter (yet?)

                // These are not normal to occur regularly, so thresholds can be tighter
                BootReason::HardFault => {
                    fault_total = fault_total.saturating_add(1).min(COUNT_MASK);
                    write(registers, Stat::Fault, fault_total);

                    fault_total > 5
                }
                BootReason::Watchdog => {
                    wdt_total = wdt_total.saturating_add(1).min(COUNT_MASK);
                    write(registers, Stat::WDT, wdt_total);

                    wdt_total > 5
                }
                BootReason::Panic => {
                    panic_total = panic_total.saturating_add(1).min(COUNT_MASK);
                    write(registers, Stat::Panic, panic_total);

                    panic_total > 5
                }

                BootReason::Unknown => boot_total > 10, // Note: no separate stat counter (yet?)

                // unreachable?
                BootReason::Loop => true,
            };
            if loop_found {
                boot_reason = BootReason::Loop;
                Self::reset(registers)?;
            }
        }

        Ok(Self {
            boot_reason,
            uptime_total,
            boot_total,
            por_total,
            wdt_total,
            panic_total,
            fault_total,
        })
    }

    /// Try to reset the stats to zero
    ///
    /// Fails if the array is too short ( < Stat::COUNT )
    pub fn reset<U32: crate::traits::MutU32>(registers: &mut [U32]) -> Result<(), ()> {
        if registers.len() < Stat::COUNT {
            return Err(());
        }

        // Clear all registers
        for reg in Stat::iter() {
            write(registers, reg, 0);
        }

        // Set FLAG on uptime counters to mark the stats as valid
        write(registers, Stat::UptimeTotal, FLAG);
        write(registers, Stat::UptimeCurrent, FLAG);

        Ok(())
    }
}

#[cfg(test)]
mod test {

    use super::*;

    #[derive(Clone, Copy)]
    struct DummyReg {
        reg: u32,
    }

    impl DummyReg {
        const fn new(init: u32) -> Self {
            Self { reg: init }
        }
    }

    impl crate::traits::U32 for DummyReg {
        fn read(&self) -> u32 {
            self.reg
        }
    }

    impl crate::traits::MutU32 for DummyReg {
        fn write(&mut self, new_value: u32) {
            self.reg = new_value
        }
    }

    #[test]
    fn test_default_stats_empty() {
        const DEFAULT_00: DummyReg = DummyReg::new(0);
        const DEFAULT_FF: DummyReg = DummyReg::new(0xFF);
        for dummy_value in [DEFAULT_00, DEFAULT_FF] {
            let mut regs = [dummy_value; 7];

            // registers are not in valid state: try_from should correctly reset them
            let stats = Stats::try_from_registers(&mut regs).unwrap();

            // No info known: report unknown, 1st boot
            assert_eq!(BootReason::Unknown, stats.boot_reason);
            assert_eq!(0, stats.uptime_total);
            assert_eq!(1, stats.boot_total);
            assert_eq!(0, stats.por_total);
            assert_eq!(0, stats.wdt_total);
            assert_eq!(0, stats.panic_total);
            assert_eq!(0, stats.fault_total);
        }
    }

    #[test]
    fn test_wdt_flag() {
        const DEFAULT: DummyReg = DummyReg::new(0);
        let mut regs = [DEFAULT; 7];
        Stats::reset(&mut regs).unwrap();

        // Set watchdog flag
        set_flag(&mut regs[Stat::WDT as usize]);

        let stats = Stats::try_from_registers(&mut regs).unwrap();

        // 1st boot: Watchdog flag set
        assert_eq!(BootReason::Watchdog, stats.boot_reason);
        assert_eq!(0, stats.uptime_total);
        assert_eq!(1, stats.boot_total);
        assert_eq!(0, stats.por_total);
        assert_eq!(1, stats.wdt_total);
        assert_eq!(0, stats.panic_total);
        assert_eq!(0, stats.fault_total);
    }

    #[test]
    fn test_por_and_wdt_flag() {
        const DEFAULT: DummyReg = DummyReg::new(0);
        let mut regs = [DEFAULT; 7];
        Stats::reset(&mut regs).unwrap();

        // First session: recover after watchdog reset
        set_flag(&mut regs[Stat::WDT as usize]);
        let _stats = Stats::try_from_registers(&mut regs).unwrap();

        // Simulate +500 seconds uptime
        increment_by(&mut regs[Stat::UptimeCurrent as usize], 500);

        // -- simulate power cycle //
        set_flag(&mut regs[Stat::POR as usize]);
        let stats = Stats::try_from_registers(&mut regs).unwrap();
        assert_eq!(BootReason::PowerOn, stats.boot_reason);

        // Simulate +100 seconds uptime
        increment_by(&mut regs[Stat::UptimeCurrent as usize], 100);

        // -- watchdog reboot -- //
        set_flag(&mut regs[Stat::WDT as usize]);
        let stats = Stats::try_from_registers(&mut regs).unwrap();

        // Boot cycle 3: Watchdog flag set
        assert_eq!(BootReason::Watchdog, stats.boot_reason);
        assert_eq!(600, stats.uptime_total);
        assert_eq!(3, stats.boot_total);
        assert_eq!(1, stats.por_total);
        assert_eq!(2, stats.wdt_total);
        assert_eq!(0, stats.panic_total);
        assert_eq!(0, stats.fault_total);
    }

    #[test]
    fn test_multiple_flags_set() {
        const DEFAULT: DummyReg = DummyReg::new(0);
        let mut regs = [DEFAULT; 7];
        Stats::reset(&mut regs).unwrap();

        // First session: recover after panic during hardfault
        set_flag(&mut regs[Stat::Fault as usize]);
        set_flag(&mut regs[Stat::Panic as usize]);
        let stats = Stats::try_from_registers(&mut regs).unwrap();

        assert_eq!(BootReason::HardFault, stats.boot_reason);
        assert_eq!(0, stats.uptime_total);
        assert_eq!(1, stats.boot_total);
        assert_eq!(0, stats.por_total);
        assert_eq!(0, stats.wdt_total);
        assert_eq!(0, stats.panic_total);
        assert_eq!(1, stats.fault_total);
    }
}
