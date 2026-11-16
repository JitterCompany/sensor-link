fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/linker");

    // The app and the bootloader pick a script from here by name
    // (standalone.x, bootloader.x or bootloadable.x)
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-search={manifest_dir}/src/linker");
}
