//! Gesture-only workstation file snapshots, transported as opaque OSC5522 bytes.
use crate::clipboard::{ClipboardData, ClipboardError, ClipboardOffer, ReaderPermit};
use promise::{Future, Promise};
use std::convert::TryFrom;
use std::fs::OpenOptions;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Private versioned encoding agreed with Tau; URI-list itself remains metadata.
pub const FILE_BUNDLE_MIME: &str = "application/vnd.tau.files-v1";
/// Current Tau aggregate decoded clipboard cap, including bundle framing.
pub const FILE_BUNDLE_LIMIT: usize = 16 * 1024 * 1024;
const URI_LIST: &str = "text/uri-list";
const GNOME_FILES: &str = "x-special/gnome-copied-files";
const MAGIC: &[u8; 9] = b"TAUFILES\0";

/// A native gesture offer augmented with one bounded snapshot, never an ambient read.
struct FileOffer {
    /// Exact compositor offer, retaining its ownership validity.
    native: Arc<dyn ClipboardOffer>,
    /// Native inventory plus the private file-body format.
    types: Vec<String>,
    /// Whole validated envelope, shared by response allocations.
    bundle: Arc<Vec<u8>>,
}

impl ClipboardOffer for FileOffer {
    fn mime_types(&self) -> &[String] {
        &self.types
    }

    fn valid(&self) -> bool {
        self.native.valid()
    }

    fn validity_check(&self) -> Option<Arc<dyn Fn() -> bool + Send + Sync>> {
        let native = self.native.clone();
        Some(Arc::new(move || native.valid()))
    }

    fn read(
        &self,
        mut types: Vec<String>,
        limit: usize,
        valid: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Future<ClipboardData> {
        let wants_bundle = types.iter().any(|t| t == FILE_BUNDLE_MIME);
        types.retain(|t| t != FILE_BUNDLE_MIME);
        if !self.valid() || !valid() {
            return Future::err(ClipboardError::Stale.into());
        }
        let native = self.native.clone();
        let bundle = self.bundle.clone();
        let remaining = if wants_bundle {
            match limit.checked_sub(bundle.len()) {
                Some(remaining) => remaining,
                None => return Future::err(ClipboardError::Limit.into()),
            }
        } else {
            limit
        };
        let mut promise = Promise::new();
        let result = promise.get_future().unwrap();
        promise::spawn::spawn(async move {
            promise.result(
                async {
                    let mut data = if types.is_empty() {
                        ClipboardData::new()
                    } else {
                        native.read(types, remaining, valid.clone()).await?
                    };
                    if !native.valid() || !valid() {
                        return Err(ClipboardError::Stale.into());
                    }
                    if wants_bundle {
                        data.insert(FILE_BUNDLE_MIME.to_owned(), bundle);
                    }
                    Ok(data)
                }
                .await,
            );
        })
        .detach();
        result
    }
}

/// Adds file bodies only when called by the enabled, original native paste gesture.
///
/// Native clipboard acquisition and local file work share four worker slots.
/// Deadline/cancellation checks occur between regular-file reads: a blocking
/// filesystem syscall cannot be forcibly interrupted by this worker.
pub fn snapshot_offer(
    native: Arc<dyn ClipboardOffer>,
    limit: usize,
    valid: Arc<dyn Fn() -> bool + Send + Sync>,
) -> Future<Arc<dyn ClipboardOffer>> {
    let metadata = if native.mime_types().iter().any(|m| m == URI_LIST) {
        URI_LIST
    } else if native.mime_types().iter().any(|m| m == GNOME_FILES) {
        GNOME_FILES
    } else {
        return Future::ok(native);
    };
    let limit = limit.min(FILE_BUNDLE_LIMIT);
    let deadline = Instant::now() + Duration::from_secs(10);
    let acquisition = native.read(vec![metadata.to_owned()], limit, valid.clone());
    let mut promise = Promise::new();
    let result = promise.get_future().unwrap();
    promise::spawn::spawn(async move {
        let data = match acquisition.await {
            Ok(data) => data,
            Err(err) => {
                promise.err(err);
                return;
            }
        };
        let permit = match ReaderPermit::acquire() {
            Ok(permit) => permit,
            Err(err) => {
                promise.err(err.into());
                return;
            }
        };
        std::thread::spawn(move || {
            let _permit = permit;
            let result = (|| {
                let check = || {
                    if !native.valid() || !valid() {
                        return Err(ClipboardError::Stale.into());
                    }
                    if Instant::now() >= deadline {
                        return Err(ClipboardError::Temporary.into());
                    }
                    Ok(())
                };
                check()?;
                let bytes = data.get(metadata).ok_or(ClipboardError::Temporary)?;
                finish_snapshot(native.clone(), bytes, metadata, limit, &check)
            })();
            promise.result(result);
        });
    })
    .detach();
    result
}

fn denied() -> anyhow::Error {
    ClipboardError::Limit.into()
}

fn finish_snapshot(
    native: Arc<dyn ClipboardOffer>,
    bytes: &[u8],
    metadata: &str,
    limit: usize,
    check: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<Arc<dyn ClipboardOffer>> {
    let paths = match parse_paths(bytes, metadata)? {
        Some(paths) => paths,
        // A copied browser URL is not a request to acquire local files.
        None => {
            check()?;
            return Ok(native);
        }
    };
    let bundle = build_bundle(&paths, limit, check)?;
    check()?;
    let mut types = native.mime_types().to_vec();
    types.retain(|t| t != FILE_BUNDLE_MIME);
    types.push(FILE_BUNDLE_MIME.to_owned());
    Ok(Arc::new(FileOffer {
        native,
        types,
        bundle: Arc::new(bundle),
    }))
}

fn parse_paths(bytes: &[u8], metadata: &str) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let text = std::str::from_utf8(bytes).map_err(|_| denied())?;
    let mut lines = text.lines();
    if metadata == GNOME_FILES && !matches!(lines.next(), Some("copy" | "cut")) {
        return Err(denied());
    }
    let mut paths = Vec::new();
    let mut non_file = false;
    for line in lines {
        if line.is_empty() || (metadata == URI_LIST && line.starts_with('#')) {
            continue;
        }
        // URL parsers can preserve malformed percent escapes; reject them
        // rather than accidentally opening a different local filename.
        let mut chars = line.bytes();
        while let Some(b) = chars.next() {
            if b == b'%'
                && !(chars.next().is_some_and(|b| b.is_ascii_hexdigit())
                    && chars.next().is_some_and(|b| b.is_ascii_hexdigit()))
            {
                return Err(denied());
            }
        }
        let uri = url::Url::parse(line).map_err(|_| denied())?;
        if uri.scheme() != "file" {
            if metadata == URI_LIST {
                non_file = true;
                continue;
            }
            return Err(denied());
        }
        if uri
            .host_str()
            .is_some_and(|h| h != "localhost" && !h.is_empty())
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.port().is_some()
            || uri.query().is_some()
            || uri.fragment().is_some()
        {
            return Err(denied());
        }
        let path = uri.to_file_path().map_err(|_| denied())?;
        if !path.is_absolute() || path.as_os_str().as_bytes().contains(&0) {
            return Err(denied());
        }
        paths.push(path);
    }
    if non_file {
        return if paths.is_empty() {
            Ok(None)
        } else {
            Err(denied())
        };
    }
    if paths.is_empty() {
        return Err(denied());
    }
    Ok(Some(paths))
}

fn build_bundle(
    paths: &[PathBuf],
    limit: usize,
    check: &dyn Fn() -> anyhow::Result<()>,
) -> anyhow::Result<Vec<u8>> {
    let count = u32::try_from(paths.len()).map_err(|_| denied())?;
    if paths.is_empty() || limit < MAGIC.len() + 4 {
        return Err(denied());
    }
    let mut bundle = MAGIC.to_vec();
    append(&mut bundle, &count.to_be_bytes(), limit)?;
    let mut buffer = [0; 8192];
    for path in paths {
        check()?;
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(denied)?;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name
                .chars()
                .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        {
            return Err(denied());
        }
        let name_len = u16::try_from(name.len()).map_err(|_| denied())?;
        let header_size = 12usize.checked_add(name.len()).ok_or_else(denied)?;
        if header_size > limit.saturating_sub(bundle.len()) {
            return Err(denied());
        }
        // Resolve ordinary symlinks without invoking a device/FIFO open method.
        // Validate this O_PATH descriptor, then open that same pinned regular
        // file through procfs, never re-resolve the owner-supplied path.
        let path_file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open(path)?;
        let metadata = path_file.metadata()?;
        if !metadata.is_file() {
            return Err(denied());
        }
        if metadata.len() > (limit - bundle.len() - header_size) as u64 {
            return Err(denied());
        }
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOCTTY)
            .open(format!("/proc/self/fd/{}", path_file.as_raw_fd()))?;
        append(&mut bundle, &name_len.to_be_bytes(), limit)?;
        append(&mut bundle, &0u16.to_be_bytes(), limit)?; // No MIME guessing.
        let length_pos = bundle.len();
        append(&mut bundle, &0u64.to_be_bytes(), limit)?;
        append(&mut bundle, name.as_bytes(), limit)?;
        let body_pos = bundle.len();
        loop {
            check()?;
            let n = match file.read(&mut buffer) {
                Ok(n) => n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            };
            if n == 0 {
                break;
            }
            if n > limit.saturating_sub(bundle.len()) {
                return Err(denied());
            }
            append(&mut bundle, &buffer[..n], limit)?;
        }
        // The actual read length is authoritative, not a pre-read stat snapshot.
        let body_len = (bundle.len() - body_pos) as u64;
        bundle[length_pos..length_pos + 8].copy_from_slice(&body_len.to_be_bytes());
    }
    check()?;
    // Queued responses account decoded bytes, so retained backing capacity
    // must equal length rather than geometrically rounded worker capacity.
    Ok(bundle.into_boxed_slice().into_vec())
}

fn append(bundle: &mut Vec<u8>, bytes: &[u8], limit: usize) -> anyhow::Result<()> {
    let length = bundle.len().checked_add(bytes.len()).ok_or_else(denied)?;
    if length > limit {
        return Err(denied());
    }
    if length > bundle.capacity() {
        // Keep geometric growth off the retained path, and never request a
        // worker backing allocation larger than the aggregate policy cap.
        let capacity = length.max(bundle.capacity().saturating_mul(2)).min(limit);
        bundle.reserve_exact(capacity - bundle.len());
    }
    bundle.extend_from_slice(bytes);
    Ok(())
}

#[cfg(test)]
mod tests;
