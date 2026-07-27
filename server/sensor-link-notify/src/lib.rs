pub mod alarm;
pub mod mail;
pub mod sms;

#[repr(u32)]
pub enum EventCodes {
    AlarmSMS = 201,
    SMS = 202,
    Email = 203,
    ErrorSendingSMS = 410,
}
