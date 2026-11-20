//! Keeps the `firmware` table in sync with the firmware images on disk.
//!
//! Signed images are uploaded to the host directory that the `firmware-files`
//! container serves (see `deploy/docker-compose.yml`); nothing else creates the
//! matching database rows. This task scans that directory whenever it changes
//! and inserts a row for every image that has none yet, so an uploaded image
//! shows up in the dashboard without a manual database write.
//!
//! The rows follow the files both ways: a row whose image is gone is removed —
//! down to the last row when the directory holds no images at all — so a device
//! can never be marked for an update whose download URL 404s (a device still
//! pointing at a removed row simply gets no announcement). A row deleted by hand
//! while its image is there comes back on the next scan.

use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use chrono::Utc;
use demo_protocol::DeviceType;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use sensor_link_server_core::store_traits::FirmwareStore;
use task_supervisor::{get_crate_relative_function_path, Handle, PanicCallback};
use tokio::sync::{mpsc, watch};

use crate::device::NewFirmware;

/// How often the firmware directory is rescanned without a filesystem event.
///
/// Scanning is event-driven; this is the safety net for a missed or unsupported
/// event (and the reason a lost event costs minutes, not forever).
const FALLBACK_SCAN_INTERVAL: Duration = Duration::from_secs(300);

/// Minimum age of a file before it is picked up.
///
/// Images typically arrive over `scp`, which
/// writes them under their final name while still uploading. Ignoring
/// just-touched files keeps a half-uploaded image from being published as a
/// download URL devices could pull.
const MIN_FILE_AGE: Duration = Duration::from_secs(30);

/// Extension of the signed images that are uploaded and served.
const FIRMWARE_FILE_EXT: &str = ".bin.sig.b64";

pub fn start_task<DB>(db: DB, on_panic: PanicCallback) -> Handle
where
    DB: FirmwareStore<DeviceType = DeviceType> + Clone,
{
    let task_function = firmware_sync_task;
    Handle::new(
        move |shutdown_rx| task_function(shutdown_rx, db.clone()),
        get_crate_relative_function_path(task_function),
        on_panic,
    )
}

async fn firmware_sync_task<DB: FirmwareStore<DeviceType = DeviceType>>(
    mut shutdown_rx: watch::Receiver<()>,
    db: DB,
) {
    let dir = crate::get_firmware_dir();
    let base_url = crate::get_firmware_base_url();
    tracing::info!(
        "Watching firmware directory {} (served at {base_url})",
        dir.display()
    );

    // Dropping the watcher stops the events, so it lives as long as the task.
    let (change_tx, mut change_rx) = mpsc::channel(1);
    let _watcher = watch_dir(&dir, change_tx);

    let mut interval = tokio::time::interval(FALLBACK_SCAN_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => break,
            _ = interval.tick() => {}
            Some(()) = change_rx.recv() => {
                // Let the upload that triggered the event finish, and coalesce
                // the events it produces in the meantime (see MIN_FILE_AGE).
                if !sleep_unless_shutdown(&mut shutdown_rx, MIN_FILE_AGE).await {
                    break;
                }
                while change_rx.try_recv().is_ok() {}
            }
        }
        if let Err(err) = sync_firmware_dir(&db, &dir, &base_url).await {
            tracing::error!("Failed to sync firmware directory {}: {err}", dir.display());
        }
    }
}

/// Send `()` on `change_tx` whenever something in `dir` changes.
///
/// Events are only a hint that the directory is worth rescanning, so they are
/// coalesced into a single pending notification and their contents ignored.
/// Returns `None` if the directory cannot be watched (it does not exist yet,
/// or the platform has no watcher for it): scanning then falls back to
/// [`FALLBACK_SCAN_INTERVAL`] alone.
fn watch_dir(dir: &Path, change_tx: mpsc::Sender<()>) -> Option<RecommendedWatcher> {
    let mut watcher = notify::recommended_watcher(move |event| match event {
        // A full channel already holds a pending notification.
        Ok(_) => drop(change_tx.try_send(())),
        Err(err) => tracing::warn!("Firmware directory watch error: {err}"),
    })
    .inspect_err(|err| tracing::warn!("Failed to create firmware directory watcher: {err}"))
    .ok()?;
    watcher
        .watch(dir, RecursiveMode::NonRecursive)
        .inspect_err(|err| tracing::warn!("Failed to watch {}: {err}", dir.display()))
        .ok()?;
    Some(watcher)
}

/// Sleep for `duration`; `false` means shutdown was signalled instead.
async fn sleep_unless_shutdown(shutdown_rx: &mut watch::Receiver<()>, duration: Duration) -> bool {
    tokio::select! {
        _ = shutdown_rx.changed() => false,
        _ = tokio::time::sleep(duration) => true,
    }
}

/// Make the `firmware` rows match the images in `dir`: insert one per image
/// that has none yet, and remove the rows whose image is gone — a device must
/// not be pointed at a download URL that 404s.
///
/// Rows are matched to files by their download URL, which is what devices are
/// handed and is unique per file name.
async fn sync_firmware_dir<DB: FirmwareStore<DeviceType = DeviceType>>(
    db: &DB,
    dir: &Path,
    base_url: &str,
) -> anyhow::Result<()> {
    // A directory that cannot be read (missing, not mounted) fails the whole
    // scan rather than reading as empty, so the rows survive it; a readable but
    // empty directory does empty the table.
    let files = firmware_files(dir, SystemTime::now())?;

    let firmwares = db
        .get_firmwares(&DeviceType::Demo)
        .await
        .map_err(|err| anyhow::anyhow!("Failed to read firmwares: {err:?}"))?;

    for file in files.iter().filter(|file| file.settled) {
        let url = download_url(base_url, &file.name);
        if firmwares.iter().any(|firmware| firmware.v2BinID == url) {
            continue;
        }
        let Some(version) = version_from_file_name(&file.name) else {
            tracing::warn!("Skipping firmware file with unexpected name: {}", file.name);
            continue;
        };
        tracing::info!("Adding firmware {version} for {}", file.name);
        db.insert_firmware(NewFirmware {
            version,
            description: String::new(),
            date: Utc::now(),
            v2BinID: url,
            recommended: false,
            device_type: DeviceType::Demo,
        })
        .await
        .map_err(|err| anyhow::anyhow!("Failed to insert firmware for {}: {err:?}", file.name))?;
    }

    // Files still being uploaded count as present here: their row (if any, from
    // an earlier upload under the same name) should survive the rewrite.
    let urls: Vec<String> = files
        .iter()
        .map(|file| download_url(base_url, &file.name))
        .collect();
    for firmware in firmwares
        .iter()
        .filter(|firmware| !urls.contains(&firmware.v2BinID))
    {
        tracing::info!(
            "Removing firmware {} ({}): file is gone",
            firmware.version,
            firmware.v2BinID
        );
        db.remove_firmware(&firmware.id)
            .await
            .map_err(|err| anyhow::anyhow!("Failed to remove firmware {}: {err:?}", firmware.id))?;
    }
    Ok(())
}

/// A firmware image in the watched directory.
struct FirmwareFile {
    name: String,
    /// Whether the file is old enough to publish (see [`MIN_FILE_AGE`]).
    settled: bool,
}

/// The firmware images in `dir`; `now` is the reference point for their age.
fn firmware_files(dir: &Path, now: SystemTime) -> std::io::Result<Vec<FirmwareFile>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with(FIRMWARE_FILE_EXT) {
            continue;
        }
        let metadata = entry.metadata()?;
        if !metadata.is_file() {
            continue;
        }
        // An unreadable or future modification time counts as settled: the age
        // check only exists to skip uploads still in progress.
        let age = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok());
        let settled = age.is_none_or(|age| age >= MIN_FILE_AGE);
        files.push(FirmwareFile { name, settled });
    }
    Ok(files)
}

fn download_url(base_url: &str, file_name: &str) -> String {
    format!("{}/{file_name}", base_url.trim_end_matches('/'))
}

/// Version of a firmware file, e.g. `0.1.1-42-abc1234` for
/// `demo-v0.1.1-42-abc1234-production.bin.sig.b64`.
///
/// Signed images are expected to be named
/// `<binary>-v<version>-<git rev>-<suffix>.bin.sig.b64`; everything up to the
/// suffix is kept, so the result has the same `<version>-<git rev>` shape as
/// the firmware version a device reports (see `device::info_handler`) and the
/// two can be compared. Returns `None` for a name that does not follow the
/// scheme.
fn version_from_file_name(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(FIRMWARE_FILE_EXT)?;
    let (_binary, rest) = stem.split_once("-v")?;
    // The version itself may contain dashes (`0.1.1-42`, `0.1.1-dev`), so only
    // the suffix is split off, from the end.
    let (version, _suffix) = rest.rsplit_once('-')?;
    // A name without a git revision is not one of ours.
    let has_git_rev = version
        .rsplit_once('-')
        .is_some_and(|(_, rev)| !rev.is_empty());
    has_git_rev.then(|| version.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        path::PathBuf,
        time::{Duration, SystemTime},
    };

    use crate::test_support::MockFirmwareStore;

    use super::*;

    const BASE_URL: &str = "https://mqtt.demo.example/fw";

    fn write_file(dir: &Path, file_name: &str) {
        std::fs::write(dir.join(file_name), "signature").unwrap();
    }

    /// Temp dir holding firmware files, removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!("demo-firmware-test-{name}"));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn version_is_parsed_from_the_file_name() {
        assert_eq!(
            version_from_file_name("demo-v0.1.1-42-abc1234-production.bin.sig.b64").as_deref(),
            Some("0.1.1-42-abc1234")
        );
        assert_eq!(
            version_from_file_name("demo-v0.1.1-dev-da96c1dd-debug.bin.sig.b64").as_deref(),
            Some("0.1.1-dev-da96c1dd")
        );
    }

    #[test]
    fn unexpected_file_names_have_no_version() {
        assert_eq!(version_from_file_name("demo.bin.sig.b64"), None);
        assert_eq!(
            version_from_file_name("demo-v0.1.1-abc1234.bin.sig.b64"),
            None
        );
        assert_eq!(
            version_from_file_name("demo-v0.1.1-42-abc1234-production.bin"),
            None
        );
    }

    #[test]
    fn only_firmware_files_are_listed_and_recent_ones_are_unsettled() {
        let dir = TempDir::new("settled");
        write_file(dir.path(), "demo-v0.1.1-42-abc1234-production.bin.sig.b64");
        write_file(dir.path(), "demo.bin");
        write_file(dir.path(), "notes.txt");

        // The files were just written, so they are still within MIN_FILE_AGE.
        let files = firmware_files(dir.path(), SystemTime::now()).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(
            files[0].name,
            "demo-v0.1.1-42-abc1234-production.bin.sig.b64"
        );
        assert!(!files[0].settled);

        let later = SystemTime::now() + MIN_FILE_AGE + Duration::from_secs(1);
        assert!(firmware_files(dir.path(), later).unwrap()[0].settled);
    }

    #[tokio::test]
    async fn a_row_is_inserted_for_an_image_without_one() {
        let dir = TempDir::new("insert");
        write_file(dir.path(), "demo-v0.1.1-42-abc1234-production.bin.sig.b64");
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();

        let inserted = db.inserted();
        assert_eq!(inserted.len(), 1);
        assert_eq!(inserted[0].version, "0.1.1-42-abc1234");
        assert_eq!(inserted[0].description, "");
        assert_eq!(
            inserted[0].v2BinID,
            "https://mqtt.demo.example/fw/demo-v0.1.1-42-abc1234-production.bin.sig.b64"
        );
        assert!(!inserted[0].recommended);
        assert_eq!(inserted[0].device_type, DeviceType::Demo);
    }

    #[tokio::test]
    async fn an_image_that_already_has_a_row_is_left_alone() {
        let dir = TempDir::new("existing");
        write_file(dir.path(), "demo-v0.1.1-42-abc1234-production.bin.sig.b64");
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();
        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();

        assert_eq!(db.inserted().len(), 1);
    }

    #[tokio::test]
    async fn the_row_of_a_removed_image_is_removed() {
        let dir = TempDir::new("removed");
        let gone = "demo-v0.1.1-42-abc1234-production.bin.sig.b64";
        write_file(dir.path(), gone);
        write_file(dir.path(), "demo-v0.1.2-43-def5678-production.bin.sig.b64");
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();
        assert_eq!(db.stored().len(), 2);

        std::fs::remove_file(dir.path().join(gone)).unwrap();
        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();

        let stored = db.stored();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].version, "0.1.2-43-def5678");
    }

    #[tokio::test]
    async fn an_image_being_uploaded_keeps_the_row_of_its_predecessor() {
        let dir = TempDir::new("reupload");
        let file_name = "demo-v0.1.1-42-abc1234-production.bin.sig.b64";
        write_file(dir.path(), file_name);
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();
        // Rewritten under the same name, so it is unsettled again.
        write_file(dir.path(), file_name);
        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();

        assert_eq!(db.stored().len(), 1);
    }

    #[tokio::test]
    async fn an_empty_directory_empties_the_table() {
        let dir = TempDir::new("empty");
        let file_name = "demo-v0.1.1-42-abc1234-production.bin.sig.b64";
        write_file(dir.path(), file_name);
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();
        std::fs::remove_file(dir.path().join(file_name)).unwrap();
        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();

        assert!(db.stored().is_empty());
    }

    #[tokio::test]
    async fn an_unreadable_directory_leaves_the_table_alone() {
        let dir = TempDir::new("unreadable");
        write_file(dir.path(), "demo-v0.1.1-42-abc1234-production.bin.sig.b64");
        filetime_backdate(dir.path());
        let db = MockFirmwareStore::new();

        sync_firmware_dir(&db, dir.path(), BASE_URL).await.unwrap();
        let missing = dir.path().join("not-mounted");
        sync_firmware_dir(&db, &missing, BASE_URL)
            .await
            .unwrap_err();

        assert_eq!(db.stored().len(), 1);
    }

    /// Set the modification time of every file in `dir` past [`MIN_FILE_AGE`],
    /// so a scan at the current time picks them up.
    fn filetime_backdate(dir: &Path) {
        let past = SystemTime::now() - MIN_FILE_AGE - Duration::from_secs(1);
        for entry in std::fs::read_dir(dir).unwrap() {
            let file = std::fs::File::options()
                .write(true)
                .open(entry.unwrap().path())
                .unwrap();
            file.set_modified(past).unwrap();
        }
    }
}
