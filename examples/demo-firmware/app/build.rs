use std::{env, path::Path};

use demo_firmware_core::{
    meta::firmware::{ApplicationHeader, DeviceType},
    utils::file,
};

/// Reads an environment variable and reruns the build script when it changes
fn env_var(key: &str) -> Result<String, env::VarError> {
    println!("cargo:rerun-if-env-changed={key}");
    env::var(key)
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if env_var("CARGO_FEATURE_DISABLE_SLEEP").is_ok() {
        println!(
            "cargo:warning=disable-sleep is set: power consumption will be higher than usual!"
        );
    }

    let link_type = match env_var("CARGO_FEATURE_FORCE_STANDALONE") {
        Ok(_) => {
            println!("cargo:warning=force-standalone is set: the app links as standalone!");
            "standalone"
        }
        Err(_) => "bootloadable",
    };
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let linker_dir = Path::new(&manifest_dir).join("linker").join(link_type);
    println!("cargo:rerun-if-changed={}/memory.x", linker_dir.display());
    println!("cargo:rustc-link-arg-bin=demo=-L{}", linker_dir.display());

    // Placeholder header, filled in (length, signature) by the signer
    let mut app_meta_bytes = [0; ApplicationHeader::BINARY_SIZE];
    ApplicationHeader::placeholder(DeviceType::Demo)
        .as_bytes(&mut app_meta_bytes)
        .unwrap();
    file::write_file("flash_app_meta.bin", &app_meta_bytes);

    // The broker CA is included at compile time, from a file that is not committed
    let ca = env_var("DEMO_MQTT_CA").expect("DEMO_MQTT_CA is not set");
    if !Path::new(&ca).is_file() {
        panic!("DEMO_MQTT_CA: no file at {ca}. Run scripts/gen-dev-keys.sh first.");
    }
    println!("cargo:rerun-if-changed={ca}");
    for key in [
        "DEMO_MQTT_HOST",
        "DEMO_MQTT_PORT",
        "BUILD_VERSION",
        "GIT_REV",
    ] {
        let _ = env_var(key);
    }
}
