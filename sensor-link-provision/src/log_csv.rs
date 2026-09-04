//! Append-only issuance log: one row per provisioned device.

use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result};

pub const HEADER: &str = "datetime_utc,uid,sim_icc,cert_serial,cert_sha256";

pub struct Row<'a> {
    pub uid: &'a str,
    pub icc: &'a str,
    pub cert_serial: &'a str,
    pub cert_sha256: &'a str,
}

/// Create the file with a header when it does not exist yet.
pub fn init(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    if !path.exists() {
        fs::write(path, format!("{HEADER}\n"))
            .with_context(|| format!("creating {}", path.display()))?;
    }
    Ok(())
}

pub fn append(path: &Path, row: &Row<'_>) -> Result<()> {
    let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    let mut f = OpenOptions::new()
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(
        f,
        "{ts},{},{},{},{}",
        row.uid, row.icc, row.cert_serial, row.cert_sha256
    )?;
    f.flush()?;
    Ok(())
}

pub fn contains_uid(path: &Path, uid: &str) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines()
        .skip(1)
        .any(|l| l.split(',').nth(1) == Some(uid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("issuance.csv");
        init(&p).unwrap();
        init(&p).unwrap();
        assert!(!contains_uid(&p, "ABC"));
        append(
            &p,
            &Row {
                uid: "ABC",
                icc: "1",
                cert_serial: "0A",
                cert_sha256: "ff",
            },
        )
        .unwrap();
        assert!(contains_uid(&p, "ABC"));
        assert!(!contains_uid(&p, "AB"));
        let text = fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 2);
        assert_eq!(text.lines().next().unwrap(), HEADER);
    }
}
