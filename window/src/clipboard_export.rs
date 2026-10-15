//! Explicit file exports stage opaque bytes, never resolve client-supplied paths.
use crate::clipboard::{ClipboardData, ClipboardError, ReaderPermit};
use anyhow::{ensure, Context};
use promise::{Future, Promise};
use std::convert::TryInto;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant, SystemTime};
use tempfile::TempDir;

/// Reserved payload format, interpreted only for an explicit `write-file`.
pub const FILE_EXPORT_MIME: &str = "application/vnd.tau.file-export-v1";
/// File body limit; the bounded envelope is additional.
pub const FILE_BODY_LIMIT: usize = 16 * 1024 * 1024;
const LIFETIME: Duration = Duration::from_secs(3600);
const MAX_EXPORTS: usize = 32;
static JANITOR: Once = Once::new();

/// Invalid private export data, distinct from a local filesystem failure.
#[derive(Debug, thiserror::Error)]
#[error("invalid file export")]
pub struct InvalidExport;

/// An unpublished staging directory; dropping it rolls back all local effects.
pub struct PreparedExport {
    /// Private directory containing just the validated basename.
    directory: TempDir,
    /// Prevents reaping while an unpublished export waits for the GUI.
    _lease: File,
    /// Conservative deadline captured before directory creation.
    expires: Instant,
    /// Locally generated copied-file metadata, never remote URI authority.
    pub data: ClipboardData,
}

impl PreparedExport {
    /// Rejects preparations delayed beyond their useful lifetime before publication.
    pub fn expired(&self) -> bool {
        Instant::now() >= self.expires
    }

    /// Retains published files until the documented expiry, including owner loss.
    pub fn retain(self) {
        // Process exit must not invalidate a cached URI prematurely. The bounded
        // runtime-directory cache is reaped by any running/new fork, or logout.
        let _ = self.directory.keep();
    }
}

/// Validates and stages on one of the four shared native acquisition workers.
pub fn prepare(
    data: ClipboardData,
    valid: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Future<PreparedExport> {
    let permit = match ReaderPermit::acquire() {
        Ok(permit) => permit,
        Err(err) => return Future::err(err.into()),
    };
    let mut promise = Promise::new();
    let future = promise.get_future().unwrap();
    std::thread::spawn(move || {
        let _permit = permit;
        let deadline = Instant::now() + Duration::from_secs(10);
        let check = || {
            if !valid() {
                return Err(ClipboardError::Stale.into());
            }
            if Instant::now() >= deadline {
                return Err(ClipboardError::Temporary.into());
            }
            Ok(())
        };
        let result = (|| {
            check()?;
            if data.len() != 1 {
                return Err(InvalidExport.into());
            }
            let bytes = data.get(FILE_EXPORT_MIME).ok_or(InvalidExport)?;
            let (name, body) = parse(bytes).map_err(|_| InvalidExport)?;
            let root = runtime_root()?;
            let prepared = stage(&root, name, body, &check)?;
            JANITOR.call_once(|| {
                std::thread::spawn(|| loop {
                    std::thread::sleep(Duration::from_secs(60));
                    if let Ok(root) = runtime_root() {
                        if let Ok(_lock) = lock(&root) {
                            let _ = reap(&root, SystemTime::now());
                        }
                    }
                });
            });
            Ok(prepared)
        })();
        promise.result(result);
    });
    future
}

fn parse(bytes: &[u8]) -> anyhow::Result<(&str, &[u8])> {
    ensure!(bytes.len() >= 25, "short file export");
    ensure!(&bytes[..9] == b"TAUFILES\0", "invalid file export version");
    ensure!(bytes[9..13] == 1u32.to_be_bytes(), "expected one file");
    let name_len = u16::from_be_bytes(bytes[13..15].try_into()?) as usize;
    let mime_len = u16::from_be_bytes(bytes[15..17].try_into()?) as usize;
    let body_len = u64::from_be_bytes(bytes[17..25].try_into()?);
    ensure!(
        name_len <= 255 && mime_len <= 255,
        "file metadata too large"
    );
    ensure!(body_len <= FILE_BODY_LIMIT as u64, "file body too large");
    let header = 25 + name_len + mime_len;
    ensure!(
        bytes.len() == header + body_len as usize,
        "invalid file length"
    );
    let name = std::str::from_utf8(&bytes[25..25 + name_len])?;
    ensure!(
        !name.is_empty()
            && name != "."
            && name != ".."
            && !name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\')),
        "unsafe file basename"
    );
    let mime = &bytes[25 + name_len..header];
    if !mime.is_empty() {
        let mime = std::str::from_utf8(mime)?;
        let (kind, subtype) = mime.split_once('/').context("invalid file MIME")?;
        let token = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&c))
        };
        ensure!(token(kind) && token(subtype), "invalid file MIME");
    }
    Ok((name, &bytes[header..]))
}

fn runtime_root() -> anyhow::Result<PathBuf> {
    let runtime =
        PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR").context("no runtime directory")?);
    validated_root(runtime)
}

fn validated_root(runtime: PathBuf) -> anyhow::Result<PathBuf> {
    ensure!(runtime.is_absolute(), "runtime directory must be absolute");
    let metadata = fs::symlink_metadata(&runtime)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "unsafe runtime directory"
    );
    let root = runtime.join("wezterm-file-export-v1");
    match fs::DirBuilder::new().mode(0o700).create(&root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(err) => return Err(err.into()),
    }
    let metadata = fs::symlink_metadata(&root)?;
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o077 == 0,
        "unsafe staging directory"
    );
    Ok(root)
}

fn lock(root: &Path) -> anyhow::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(root.join(".lock"))?;
    ensure!(file.metadata()?.is_file(), "invalid staging lock");
    // Never block a worker behind another process performing filesystem I/O.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(ClipboardError::Temporary.into());
    }
    Ok(file)
}

fn reap(root: &Path, now: SystemTime) -> anyhow::Result<usize> {
    let mut count = 0;
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if !entry.file_name().as_encoded_bytes().starts_with(b"export-") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path())?;
        let expired = now
            .duration_since(metadata.modified()?)
            .is_ok_and(|age| age >= LIFETIME);
        if expired && metadata.is_dir() {
            // An unpublished export can sit in the GUI queue. A per-directory
            // lease prevents deletion until that queue item publishes or drops.
            let lease = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
                .open(entry.path())?;
            if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0
                && fs::remove_dir_all(entry.path()).is_ok()
            {
                continue;
            }
        }
        count += 1;
    }
    Ok(count)
}

fn stage(
    root: &Path,
    name: &str,
    body: &[u8],
    check: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<PreparedExport> {
    let _lock = lock(root)?;
    if reap(root, SystemTime::now())? >= MAX_EXPORTS {
        return Err(ClipboardError::Temporary.into());
    }
    check()?;
    let expires = Instant::now() + LIFETIME;
    let directory = tempfile::Builder::new()
        .prefix("export-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(root)?;
    let lease = File::open(directory.path())?;
    if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        return Err(ClipboardError::Temporary.into());
    }
    let path = directory.path().join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    for chunk in body.chunks(64 * 1024) {
        check()?;
        file.write_all(chunk)?;
    }
    file.flush()?;
    check()?;
    let uri =
        url::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("invalid staging path"))?;
    let data = IntoIterator::into_iter([
        ("text/uri-list", format!("{uri}\r\n").into_bytes()),
        (
            "x-special/gnome-copied-files",
            format!("copy\n{uri}").into_bytes(),
        ),
        ("application/x-kde-cutselection", b"0".to_vec()),
    ])
    .map(|(mime, bytes)| (mime.to_owned(), Arc::new(bytes)))
    .collect();
    Ok(PreparedExport {
        directory,
        data,
        _lease: lease,
        expires,
    })
}

#[cfg(test)]
mod tests;
