//! PPP-based Quectel modem driver.
//!
//! Drop-in alternative to [`super::quectel`] behind the [`crate::mqtt::MqttClient`]
//! trait. The modem is used only as a bit-pipe: AT bring-up, then PPP data mode
//! (`ATD*99#`) with the full network stack on the MCU:
//! embassy-net (TCP/IP over PPP) → embedded-tls (mutual TLS, keys never leave
//! the MCU) → rust-mqtt (MQTT 5, QoS 1).
//!
//! This module is the only place where embedded-io-async 0.7 / heapless 0.9
//! types are used; conversions to the crate-wide 0.6 / 0.8 types happen at the
//! `MqttClient` boundary.

pub mod mqtt_core;
pub mod pem;
pub mod tls;

// Submodules land per work package:
// mod uart_adapter; WP3 — eia-0.6 UART halves → eia-0.7 BufRead+Write for PPP
// mod at_bringup;   WP5 — power/baud/APN/registration/dial/escape
// mod session;      WP7 — reusable session buffer pool
// mod ota;          WP7 (stub) / WP9 (streaming HTTP)
