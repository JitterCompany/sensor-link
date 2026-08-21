use atat::atat_derive::AtatResp;

/// Reponse format: +QHTTPGET: <err>[,<httprspcode>[,<content_length>]]
#[derive(Clone, Debug, AtatResp)]
pub struct GetResponse {
    /// Status code of the operation
    pub err: u16,

    /// HTTP response code
    pub resp: Option<u16>,

    /// The length of HTTP(S) response body. Unit: byte.
    pub content_length: Option<u32>,
}

/// Reponse format: +QHTTPREADFILE: <err>
#[derive(Clone, Debug, AtatResp)]
pub struct ReadFileResponse {
    /// Status code of the operation
    pub err: u16,
}
