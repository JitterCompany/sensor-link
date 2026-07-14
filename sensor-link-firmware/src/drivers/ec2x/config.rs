/// EC2x Config options
pub struct Config {
    /// URL to the broker
    pub host: &'static str,
    /// Port to connect on with the broker
    pub port: u16,
    /// Set to true to connect using TLS (SSL)
    pub use_tls: bool,
}
