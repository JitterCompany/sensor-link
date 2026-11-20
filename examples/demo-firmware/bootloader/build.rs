use std::{env, path::Path};

use demo_firmware_core::{
    meta::bootloader::BootloaderMeta,
    utils::{crypto, file},
};

mod meta {
    const PROD_VERSION: Option<&str> = option_env!("BUILD_VERSION");
    const DEV_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-dev");
    const GIT_REV: Option<&str> = option_env!("GIT_REV");

    pub fn fw_version() -> &'static str {
        PROD_VERSION.unwrap_or(DEV_VERSION)
    }

    pub fn git_rev() -> &'static str {
        GIT_REV.unwrap_or("dev")
    }
}

fn preprocess_meta() {
    let mut meta_bytes = [0; BootloaderMeta::BINARY_SIZE];
    BootloaderMeta::new(meta::fw_version(), meta::git_rev())
        .as_bytes(&mut meta_bytes)
        .unwrap();
    file::write_file("bootloader_meta.bin", &meta_bytes);
}

/// The public key the bootloader checks firmware signatures against. It comes
/// from a file named at build time, so no key is committed.
fn preprocess_app_pubkey() {
    println!("cargo:rerun-if-env-changed=DEMO_APP_PUBKEY");
    let path = env::var("DEMO_APP_PUBKEY").expect("DEMO_APP_PUBKEY is not set");
    if !Path::new(&path).is_file() {
        panic!("DEMO_APP_PUBKEY: no file at {path}. Run scripts/gen-dev-keys.sh first.");
    }
    println!("cargo:rerun-if-changed={path}");

    let pem = file::read_file(&path);
    let key = match crypto::PubKey::try_from_pem(pem.as_str()) {
        Ok(key) => key,
        Err(err) => panic!("Failed to preprocess public key: {err:?}"),
    };
    file::write_file("app_pubkey.bin", &key.to_bytes());
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=BUILD_VERSION");
    println!("cargo:rerun-if-env-changed=GIT_REV");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-changed=linker/memory.x");
    println!("cargo:rustc-link-arg-bins=-L{manifest_dir}/linker");

    // Serialized to binary blobs, included by main.rs
    preprocess_meta();
    preprocess_app_pubkey();
}
