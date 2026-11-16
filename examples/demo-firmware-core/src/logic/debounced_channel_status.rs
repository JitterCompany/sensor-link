use demo_protocol::status::ChannelStatus;

#[derive(Debug, Copy, Clone)]
pub struct DebouncedChannelStatus {
    status: ChannelStatus,
    prev: ChannelStatus,
    count: u8,
    max_count: u8,
}

impl DebouncedChannelStatus {
    pub const fn new(debounce_count: u8) -> Self {
        Self {
            status: ChannelStatus::Nothing,
            prev: ChannelStatus::Nothing,
            // debounce counter: counts down while state does not change
            count: 0,
            max_count: debounce_count,
        }
    }

    /// Get current (debounced) channel status
    pub fn status(&self) -> ChannelStatus {
        self.status
    }

    /// Update channel status. Returns true if changed
    pub fn update(&mut self, new: ChannelStatus) -> bool {
        // decrement debounce counter to zero
        if self.count > 0 {
            self.count -= 1;
        }
        let status_before_update = self.status;

        match (self.status, new) {
            // Change of sensor type: force immediate disconnect
            (ChannelStatus::Ok(_), ChannelStatus::Ok(_)) if new != self.status => {
                self.status = ChannelStatus::Nothing;
                self.count = self.max_count;
            }

            // No change while Ok: do nothing
            (ChannelStatus::Ok(_), ChannelStatus::Ok(_)) => {}

            // Disconnect or error: immediately update
            (ChannelStatus::Ok(_), ChannelStatus::Nothing | ChannelStatus::Error(_)) => {
                self.status = new;
                self.count = self.max_count;
            }

            // Connect or recovery/change of error: only update afer debounce
            (ChannelStatus::Nothing | ChannelStatus::Error(_), _) => {
                // debounce done: apply new status
                if self.count == 0 {
                    self.status = new;
                    self.count = self.max_count;
                // new value changed: reset debounce counter
                } else if self.prev != new {
                    self.count = self.max_count;
                }
            }
        }

        // keep track of last seen incoming status for debouncing
        self.prev = new;

        // return true if changed
        status_before_update != self.status
    }
}

#[cfg(test)]
mod tests {
    use super::DebouncedChannelStatus;
    use demo_protocol::status::{ChannelError, ChannelStatus};

    fn ok_sensor_a() -> ChannelStatus {
        ChannelStatus::Ok(demo_protocol::SensorType::BoardTemperature)
    }
    fn ok_sensor_b() -> ChannelStatus {
        ChannelStatus::Ok(demo_protocol::SensorType::Charger)
    }

    #[test]
    fn initial_is_nothing() {
        let s = DebouncedChannelStatus::new(50);
        assert_eq!(s.status(), ChannelStatus::Nothing);
    }

    #[test]
    fn ok_then_same_ok_no_change() {
        let mut s = DebouncedChannelStatus::new(50);
        let changed1 = s.update(ok_sensor_a());
        assert!(changed1);
        assert_eq!(s.status(), ok_sensor_a());
        let changed2 = s.update(ok_sensor_a());
        assert!(!changed2);
        assert_eq!(s.status(), ok_sensor_a());
    }

    #[test]
    fn change_sensor_type_forces_disconnect() {
        let mut s = DebouncedChannelStatus::new(50);
        s.update(ok_sensor_a());
        let changed = s.update(ok_sensor_b());
        assert!(changed);
        assert_eq!(s.status(), ChannelStatus::Nothing);
    }

    #[test]
    fn error_then_ok_requires_debounce() {
        let mut s = DebouncedChannelStatus::new(50);
        // go to Error immediately
        let _ = s.update(ChannelStatus::Error(ChannelError::NoSensor));
        // try recover to Ok: should not immediately change because of debounce counter
        let mut changed = s.update(ok_sensor_a());
        assert!(!changed);
        // keep feeding same value until debounce expires
        for _ in 0..50 {
            changed = s.update(ok_sensor_a());
        }
        assert!(changed);
        assert_eq!(s.status(), ok_sensor_a());
    }

    #[test]
    fn nothing_then_ok_requires_debounce_but_applies_when_counter_zero() {
        let mut s = DebouncedChannelStatus::new(50);
        // first update applies immediately (count==0)
        let changed = s.update(ok_sensor_a());
        assert!(changed);
        assert_eq!(s.status(), ok_sensor_a());
    }
}
