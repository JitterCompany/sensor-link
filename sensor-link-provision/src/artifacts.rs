//! The CI artifact zip: `provision.toml` plus bootloader and firmware ELFs.

use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use glob::Pattern;
use tempfile::TempDir;

use crate::profile::Profile;

const ELF_MAGIC: &[u8; 4] = b"\x7fELF";

#[derive(Debug, Clone)]
pub struct ArtifactFile {
    /// File name inside the zip, e.g. `bootloader-v2.0.0-31`.
    pub name: String,
    /// Extracted copy on disk (probe-rs flashes from a path).
    pub path: PathBuf,
}

pub struct Artifacts {
    pub profile: Profile,
    pub bootloader: ArtifactFile,
    /// One firmware per profile variant, same order as `profile.variants`.
    pub firmwares: Vec<ArtifactFile>,
    _dir: TempDir,
}

impl Artifacts {
    pub fn load(zip_path: &Path) -> Result<Self> {
        let file =
            File::open(zip_path).with_context(|| format!("opening {}", zip_path.display()))?;
        let mut zip = zip::ZipArchive::new(file).context("reading zip")?;
        let names: Vec<String> = zip.file_names().map(str::to_owned).collect();

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
            let fw = extract_matching(&mut zip, &names, &v.firmware, dir.path())
                .with_context(|| format!("firmware for variant '{}'", v.name))?;
            firmwares.push(fw);
        }

        Ok(Self {
            profile,
            bootloader,
            firmwares,
            _dir: dir,
        })
    }
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
        assert_eq!(a.firmwares[0].name, "zonneboiler-v0.1.0-31");
        assert_eq!(fs::read(&a.firmwares[0].path).unwrap(), fake_elf("fw"));
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
