//! Windows only: embed the app icon in the .exe so Explorer and the taskbar
//! show it. The .ico is derived from assets/jitter-icon.png at build time,
//! the same single source that macos/bundle.sh turns into the .icns.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/jitter-icon.png");
    #[cfg(windows)]
    embed_icon().expect("embed Windows icon");
}

#[cfg(windows)]
fn embed_icon() -> Result<(), Box<dyn std::error::Error>> {
    use std::path::PathBuf;

    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR")?);
    let ico = out.join("jitter-icon.ico");

    // ICO entries are at most 256x256.
    let img = image::open("assets/jitter-icon.png")?.resize_exact(
        256,
        256,
        image::imageops::FilterType::Lanczos3,
    );
    img.save_with_format(&ico, image::ImageFormat::Ico)?;

    winresource::WindowsResource::new()
        .set_icon(ico.to_str().ok_or("non-UTF-8 OUT_DIR")?)
        .compile()?;
    Ok(())
}
