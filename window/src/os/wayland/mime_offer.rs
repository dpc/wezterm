use crate::clipboard::{ClipboardData, ClipboardError, ClipboardOffer, ReaderPermit};
use promise::{Future, Promise};
use smithay_client_toolkit::data_device_manager::data_offer::SelectionOffer;
use smithay_client_toolkit::data_device_manager::ReadPipe;
use smithay_client_toolkit::primary_selection::offer::PrimarySelectionOffer;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
#[cfg(test)]
mod tests;

/// A particular protocol object, never a lookup of the current selection.
#[derive(Clone)]
pub(super) enum NativeOffer {
    Clipboard(SelectionOffer),
    Primary(PrimarySelectionOffer),
}

/// Retained Wayland offer with ownership revocation and bounded acquisition.
pub(super) struct MimeOffer {
    /// Native protocol object, absent for an empty selection.
    native: Option<NativeOffer>,
    /// Complete MIME list captured on the GUI thread.
    types: Vec<String>,
    /// Selection ownership generation shared with dispatch.
    generation: Arc<AtomicU64>,
    /// Captured generation.
    epoch: u64,
}

impl MimeOffer {
    pub(super) fn new(native: Option<NativeOffer>, generation: Arc<AtomicU64>, epoch: u64) -> Self {
        let types = match &native {
            Some(NativeOffer::Clipboard(o)) => o.with_mime_types(|m| m.to_vec()),
            Some(NativeOffer::Primary(o)) => o.with_mime_types(|m| m.to_vec()),
            None => vec![],
        };
        Self {
            native,
            types,
            generation,
            epoch,
        }
    }
}

impl ClipboardOffer for MimeOffer {
    fn mime_types(&self) -> &[String] {
        &self.types
    }

    fn valid(&self) -> bool {
        self.generation.load(Ordering::Acquire) == self.epoch
    }

    fn read(
        &self,
        types: Vec<String>,
        limit: usize,
        valid: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Future<ClipboardData> {
        let permit = match ReaderPermit::acquire() {
            Ok(permit) => permit,
            Err(err) => return Future::err(err.into()),
        };
        let mut promise = Promise::new();
        let future = promise.get_future().unwrap();
        let native = self.native.clone();
        let generation = self.generation.clone();
        let epoch = self.epoch;
        let available = self.types.clone();
        promise::spawn::spawn_into_main_thread(async move {
            if generation.load(Ordering::Acquire) != epoch || !valid() {
                promise.err(ClipboardError::Stale.into());
                return;
            }
            let mut data = ClipboardData::new();
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut remaining = limit;
            for mime in types {
                if !available.contains(&mime) || data.contains_key(&mime) {
                    continue;
                }
                if generation.load(Ordering::Acquire) != epoch || !valid() {
                    promise.err(ClipboardError::Stale.into());
                    return;
                }
                let pipe = match &native {
                    Some(NativeOffer::Clipboard(o)) => {
                        o.receive(mime.clone()).map_err(anyhow::Error::from)
                    }
                    Some(NativeOffer::Primary(o)) => {
                        o.receive(mime.clone()).map_err(anyhow::Error::from)
                    }
                    None => continue,
                };
                let pipe = match pipe {
                    Ok(pipe) => pipe,
                    Err(e) => {
                        promise.err(e);
                        return;
                    }
                };
                // Request the next format only after consuming this one. A
                // native owner is not required to support an unbounded fan-out
                // of simultaneously blocked conversion pipes.
                let mut read_promise = Promise::new();
                let read_future = read_promise.get_future().unwrap();
                let read_generation = generation.clone();
                let read_valid = valid.clone();
                std::thread::spawn(move || {
                    read_promise.result(read_bounded(
                        pipe,
                        remaining,
                        deadline,
                        &read_generation,
                        epoch,
                        &*read_valid,
                    ));
                });
                match read_future.await {
                    Ok(bytes) => {
                        remaining -= bytes.len();
                        data.insert(mime, Arc::new(bytes));
                    }
                    Err(err) => {
                        promise.err(err);
                        return;
                    }
                }
            }
            // Release acquisition capacity before waking a dependent file
            // snapshot worker, which shares the same four global slots.
            drop(permit);
            if generation.load(Ordering::Acquire) != epoch || !valid() {
                promise.err(ClipboardError::Stale.into());
            } else {
                promise.ok(data);
            }
        })
        .detach();
        future
    }
}

fn read_bounded(
    mut pipe: ReadPipe,
    limit: usize,
    deadline: Instant,
    generation: &AtomicU64,
    epoch: u64,
    valid: &dyn Fn() -> bool,
) -> anyhow::Result<Vec<u8>> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut data = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        if generation.load(Ordering::Acquire) != epoch || !valid() {
            return Err(ClipboardError::Stale.into());
        }
        if Instant::now() >= deadline {
            return Err(ClipboardError::Temporary.into());
        }
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, 100) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }
        if n == 0 {
            continue;
        }
        match pipe.read(&mut buffer) {
            Ok(0) => return Ok(data),
            Ok(n) => {
                if n > limit.saturating_sub(data.len()) {
                    return Err(ClipboardError::Limit.into());
                }
                data.extend_from_slice(&buffer[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue
            }
            Err(e) => return Err(e.into()),
        }
    }
}
