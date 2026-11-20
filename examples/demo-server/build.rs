fn main() {
    println!("cargo:rerun-if-changed=.cargo/config.toml");

    // Set build date
    let build_date = chrono::Utc::now()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string();
    println!("cargo:rustc-env=BUILD_DATE={}", build_date);
}
