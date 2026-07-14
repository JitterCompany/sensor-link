use atat::atat_derive::AtatResp;

#[derive(Clone, Debug, AtatResp)]
pub struct CREG {
    /// Registration network status.
    /// 0 Not registered. ME is not currently searching a new operator to register to
    /// 1 Registered, home network
    /// 2 Not registered, but ME is currently searching a new operator to register to
    /// 3 Registration denied
    /// 4 Unknown
    /// 5 Registered, roaming
    #[allow(dead_code)]
    pub stat: u8,
}
