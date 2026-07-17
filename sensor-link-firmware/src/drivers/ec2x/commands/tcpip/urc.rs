use atat::atat_derive::AtatResp;

#[derive(Clone, Debug, AtatResp)]
#[allow(dead_code)]
pub struct QIURC {
    pub param: heapless::String<16>,
    pub cid: u8,
}
