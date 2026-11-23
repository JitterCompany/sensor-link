//! The CI artifact zip: `provision.toml` plus bootloader and firmware ELFs, and optionally the
//! signed images of the firmwares.

use std::{
    borrow::Cow,
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use base64::{
    Engine,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use glob::Pattern;
use object::{
    Endianness,
    elf::PT_LOAD,
    read::elf::{ElfFile32, ProgramHeader},
};
use tempfile::TempDir;

use crate::profile::Profile;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";

/// A signed image may only differ from its ELF in this many leading bytes, the application
/// header that holds the signature. More differences mean the two come from different builds.
const SIGNED_HEADER_MAX: usize = 1024;

/// CI writes base64url, with or without padding.
const BASE64URL: GeneralPurpose = GeneralPurpose::new(
    &base64::alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone)]
pub struct ArtifactFile {
    /// File name inside the zip, e.g. `bootloader-v2.0.0-31`.
    pub name: String,
    /// Extracted copy on disk (probe-rs flashes from a path).
    pub path: PathBuf,
}

/// A signed raw image and where it goes in flash.
#[derive(Debug, Clone)]
pub struct SignedImage {
    /// File name inside the zip.
    pub name: String,
    /// Load address of the ELF it was built from.
    pub address: u64,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone)]
pub enum Firmware {
    Elf(ArtifactFile),
    Signed(SignedImage),
}

impl Firmware {
    pub fn name(&self) -> &str {
        match self {
            Firmware::Elf(file) => &file.name,
            Firmware::Signed(image) => &image.name,
        }
    }
}

pub struct Artifacts {
    pub profile: Profile,
    pub bootloader: ArtifactFile,
    /// One firmware per profile variant, same order as `profile.variants`.
    pub firmwares: Vec<Firmware>,
    _dir: TempDir,
}

impl Artifacts {
    pub fn load(zip_path: &Path) -> Result<Self> {
        let file =
            File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
        let mut zip = zip::ZipArchive::new(file).context("reading zip")?;
        let names = zip
            .file_names()
            .map(|name| name.map(Cow::into_owned))
            .collect::<Result<Vec<String>, _>>()
            .context("reading zip entry names")?;

        let profile_entry = names
            .iter()
            .find(|n| basename(n) == "provision.toml")
            .context("zip contains no provision.toml (is this a release artifact of a provisioning-enabled project?)")?
            .clone();
        let mut toml = String::new();
        zip.by_name(&profile_entry)?.read_to_string(&mut toml)?;
        let profile = Profile::parse(&toml)?;

        let dir = tempfile::Builder::new()
            .prefix("sensor-link-provision-")
            .tempdir()?;

        let bootloader =
            extract_matching(&mut zip, &names, &profile.artifacts.bootloader, dir.path())
                .context("bootloader")?;
        let mut firmwares = Vec::with_capacity(profile.variants.len());
        for v in &profile.variants {
            let elf = extract_matching(&mut zip, &names, &v.firmware, dir.path())
                .with_context(|| format!("firmware for variant '{}'", v.name))?;
            let firmware = match &v.signed {
                None => Firmware::Elf(elf),
                Some(pattern) => Firmware::Signed(
                    signed_image(&mut zip, &names, pattern, &elf)
                        .with_context(|| format!("signed image for variant '{}'", v.name))?,
                ),
            };
            firmwares.push(firmware);
        }

        Ok(Self {
            profile,
            bootloader,
            firmwares,
            _dir: dir,
        })
    }
}

/// Reads the single file matching `pattern` and checks it against `elf`: same load address and
/// length, and the same bytes after the header.
fn signed_image<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    names: &[String],
    pattern: &str,
    elf: &ArtifactFile,
) -> Result<SignedImage> {
    let pat = Pattern::new(pattern).with_context(|| format!("invalid glob '{pattern}'"))?;
    let matching: Vec<&String> = names.iter().filter(|n| pat.matches(basename(n))).collect();
    let entry = match matching.as_slice() {
        [one] => *one,
        [] => bail!("no file matching '{pattern}' in the zip"),
        many => bail!(
            "{} files match '{pattern}': {}",
            many.len(),
            many.iter()
                .map(|n| basename(n))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    let name = basename(entry).to_owned();
    let mut raw = Vec::new();
    zip.by_name(entry)?.read_to_end(&mut raw)?;
    let data = if name.ends_with(".b64") {
        BASE64URL
            .decode(raw.trim_ascii())
            .with_context(|| format!("{name} is not base64url"))?
    } else {
        raw
    };

    let (address, flat) = flatten_elf(&fs::read(&elf.path)?)
        .with_context(|| format!("reading the load segments of {}", elf.name))?;
    if data.len() != flat.len() {
        bail!(
            "{name} is {} bytes, but {} loads {} bytes: not the same build",
            data.len(),
            elf.name,
            flat.len()
        );
    }
    let header = SIGNED_HEADER_MAX.min(data.len());
    if data[header..] != flat[header..] {
        bail!(
            "{name} differs from {} after the header: not the same build",
            elf.name
        );
    }
    Ok(SignedImage {
        name,
        address,
        data,
    })
}

/// The load segments of an ELF as one image from the lowest physical address, gaps filled with
/// zeroes, as `objcopy -O binary` writes it.
fn flatten_elf(data: &[u8]) -> Result<(u64, Vec<u8>)> {
    let elf = ElfFile32::<Endianness>::parse(data).context("not a 32-bit ELF")?;
    let endian = elf.endian();
    let mut segments = Vec::new();
    for ph in elf.elf_program_headers() {
        if ph.p_type(endian) != PT_LOAD || ph.p_filesz(endian) == 0 {
            continue;
        }
        let bytes = ph
            .data(endian, data)
            .map_err(|_| anyhow::anyhow!("segment outside the file"))?;
        segments.push((u64::from(ph.p_paddr(endian)), bytes));
    }
    let Some(start) = segments.iter().map(|(addr, _)| *addr).min() else {
        bail!("no load segments");
    };
    let end = segments
        .iter()
        .map(|(addr, bytes)| addr + bytes.len() as u64)
        .max()
        .unwrap_or(start);
    let mut image = vec![0u8; usize::try_from(end - start)?];
    for (addr, bytes) in segments {
        let offset = usize::try_from(addr - start)?;
        image[offset..offset + bytes.len()].copy_from_slice(bytes);
    }
    Ok((start, image))
}

fn basename(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Extract the single file whose basename matches `pattern` and starts with the ELF magic
/// (`.bin`, `.b64`, `.cdx.json` siblings match the glob too but are not ELF).
fn extract_matching<R: Read + std::io::Seek>(
    zip: &mut zip::ZipArchive<R>,
    names: &[String],
    pattern: &str,
    out_dir: &Path,
) -> Result<ArtifactFile> {
    let pat = Pattern::new(pattern).with_context(|| format!("invalid glob '{pattern}'"))?;
    let mut hits = Vec::new();
    for name in names {
        let base = basename(name);
        if base.is_empty() || !pat.matches(base) {
            continue;
        }
        let mut data = Vec::new();
        zip.by_name(name)?.read_to_end(&mut data)?;
        if data.starts_with(ELF_MAGIC) {
            hits.push((base.to_owned(), data));
        }
    }
    match hits.len() {
        0 => bail!("no ELF matching '{pattern}' in the zip"),
        1 => {}
        n => bail!(
            "{n} files match '{pattern}': {}",
            hits.iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
    let (name, data) = hits.remove(0);
    let path = out_dir.join(&name);
    fs::write(&path, &data)?;
    Ok(ArtifactFile { name, path })
}

#[cfg(test)]
pub(crate) mod tests {
    use std::io::{Cursor, Write};

    use zip::write::SimpleFileOptions;

    use super::*;
    use crate::profile::EXAMPLE_PROFILE;

    /// Minimal ELF-looking blob: magic followed by filler.
    pub(crate) fn fake_elf(tag: &str) -> Vec<u8> {
        let mut v = ELF_MAGIC.to_vec();
        v.extend_from_slice(tag.as_bytes());
        v
    }

    pub(crate) fn make_zip(entries: &[(&str, &[u8])]) -> PathBuf {
        let mut buf = Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            for (name, data) in entries {
                w.start_file(*name, SimpleFileOptions::default()).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        let dir = tempfile::Builder::new()
            .prefix("ziptest-")
            .tempdir()
            .unwrap();
        let path = dir.keep().join("firmware-build-31.zip");
        fs::write(&path, buf.into_inner()).unwrap();
        path
    }

    #[test]
    fn loads_ci_layout() {
        let p = make_zip(&[
            ("provision.toml", EXAMPLE_PROFILE.as_bytes()),
            ("bootloader-v2.0.0-31", &fake_elf("bl")),
            ("bootloader-2.0.0-31.cdx.json", b"{}"),
            ("zonneboiler-v0.1.0-31", &fake_elf("fw")),
            ("zonneboiler-v0.1.0-31.bin", b"raw"),
            (
                "zonneboiler-v0.1.0-31-abcdef12-production.bin.sig.b64",
                b"xx",
            ),
        ]);
        let a = Artifacts::load(&p).unwrap();
        assert_eq!(a.bootloader.name, "bootloader-v2.0.0-31");
        let Firmware::Elf(fw) = &a.firmwares[0] else {
            panic!("expected the ELF without `signed`");
        };
        assert_eq!(fw.name, "zonneboiler-v0.1.0-31");
        assert_eq!(fs::read(&fw.path).unwrap(), fake_elf("fw"));
    }

    const LOAD_ADDRESS: u32 = 0x0802_0000;

    /// A 32-bit little-endian ELF with one load segment holding `payload` at `address`.
    fn elf_with_segment(address: u32, payload: &[u8]) -> Vec<u8> {
        const EHDR: u32 = 52;
        const PHDR: u32 = 32;
        let mut v = Vec::new();
        v.extend_from_slice(ELF_MAGIC);
        // ELFCLASS32, little endian, version 1
        v.extend_from_slice(&[1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        v.extend_from_slice(&2u16.to_le_bytes()); // ET_EXEC
        v.extend_from_slice(&40u16.to_le_bytes()); // EM_ARM
        v.extend_from_slice(&1u32.to_le_bytes());
        v.extend_from_slice(&address.to_le_bytes()); // entry
        v.extend_from_slice(&EHDR.to_le_bytes()); // phoff
        v.extend_from_slice(&0u32.to_le_bytes()); // shoff
        v.extend_from_slice(&0u32.to_le_bytes()); // flags
        for half in [EHDR as u16, PHDR as u16, 1, 40, 0, 0] {
            v.extend_from_slice(&half.to_le_bytes());
        }
        let size = payload.len() as u32;
        // PT_LOAD, offset, vaddr, paddr, filesz, memsz, flags R+X, align
        for word in [1, EHDR + PHDR, address, address, size, size, 5, 4] {
            v.extend_from_slice(&word.to_le_bytes());
        }
        v.extend_from_slice(payload);
        v
    }

    fn signed_profile() -> String {
        EXAMPLE_PROFILE
            .replace("version = 1", "version = 2")
            .replace(
                "firmware = \"zonneboiler-*\"",
                "firmware = \"zonneboiler-v*\"\nsigned = \"zonneboiler-*.bin.sig.b64\"",
            )
    }

    /// The image body plus a header that differs, as the firmware signer writes it.
    fn image(header_tag: u8) -> Vec<u8> {
        let mut data: Vec<u8> = (0..2048u32).map(|i| i as u8).collect();
        data[4..76].fill(header_tag);
        data
    }

    fn load_signed(signed: &[u8]) -> Result<Artifacts> {
        let profile = signed_profile();
        let b64 = BASE64URL.encode(signed);
        let p = make_zip(&[
            ("provision.toml", profile.as_bytes()),
            ("bootloader-v2.0.0-31", &fake_elf("bl")),
            (
                "zonneboiler-v0.1.0-31",
                &elf_with_segment(LOAD_ADDRESS, &image(0)),
            ),
            (
                "zonneboiler-v0.1.0-31-abcdef12-production.bin.sig.b64",
                b64.as_bytes(),
            ),
        ]);
        Artifacts::load(&p)
    }

    #[test]
    fn signed_image_goes_to_the_load_address() {
        let a = load_signed(&image(0xA5)).unwrap();
        let Firmware::Signed(signed) = &a.firmwares[0] else {
            panic!("expected the signed image");
        };
        assert_eq!(signed.address, u64::from(LOAD_ADDRESS));
        assert_eq!(signed.data, image(0xA5));
        assert_eq!(
            a.firmwares[0].name(),
            "zonneboiler-v0.1.0-31-abcdef12-production.bin.sig.b64"
        );
    }

    #[test]
    fn signed_image_of_another_build_is_refused() {
        let mut other = image(0xA5);
        other[1500] ^= 0xFF;
        let err = format!("{:#}", load_signed(&other).err().unwrap());
        assert!(err.contains("after the header"), "{err}");

        let mut longer = image(0xA5);
        longer.push(0);
        let err = format!("{:#}", load_signed(&longer).err().unwrap());
        assert!(err.contains("not the same build"), "{err}");
    }

    #[test]
    fn nested_dir_and_missing() {
        let p = make_zip(&[
            ("dist/provision.toml", EXAMPLE_PROFILE.as_bytes()),
            ("dist/bootloader-v2.0.0-31", &fake_elf("bl")),
        ]);
        let err = Artifacts::load(&p)
            .err()
            .expect("missing firmware must fail");
        assert!(format!("{err:#}").contains("Zonneboiler"), "{err:#}");

        let p = make_zip(&[("bootloader-v2.0.0-31", &fake_elf("bl"))]);
        assert!(Artifacts::load(&p).is_err());
    }

    #[test]
    fn ambiguous_match() {
        let p = make_zip(&[
            ("provision.toml", EXAMPLE_PROFILE.as_bytes()),
            ("bootloader-v2.0.0-31", &fake_elf("a")),
            ("bootloader-v2.0.0-32", &fake_elf("b")),
            ("zonneboiler-v0.1.0-31", &fake_elf("fw")),
        ]);
        assert!(Artifacts::load(&p).is_err());
    }
}
