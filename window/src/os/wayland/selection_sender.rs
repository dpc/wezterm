//! Bounded, multiplexed native clipboard pipe delivery; no per-format threads.
use smithay_client_toolkit::data_device_manager::WritePipe;
use std::io::Write;
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::{Duration, Instant};

const MAX_PIPES: usize = 128;
const MAX_BYTES: usize = 256 * 1024 * 1024;
static PIPES: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);
static SENDER: OnceLock<mpsc::SyncSender<PendingSend>> = OnceLock::new();

/// Charges queued and active descriptors/payload references, even across owners.
struct Permit {
    /// Conservative retained byte charge (aliases may share actual allocation).
    bytes: usize,
}
impl Drop for Permit {
    fn drop(&mut self) {
        PIPES.fetch_sub(1, Ordering::AcqRel);
        BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

/// One admitted native consumer, serviced without blocking other formats.
struct PendingSend {
    /// Owned native write endpoint.
    pipe: WritePipe,
    /// Immutable bytes from the source corresponding to this request.
    data: Arc<Vec<u8>>,
    /// Bytes already accepted by the pipe.
    offset: usize,
    /// Inactivity deadline, not an overall transfer deadline.
    deadline: Instant,
    /// Retention accounting survives queue moves.
    _permit: Permit,
}

/// Schedules a native transfer. Native overload cannot express an error through
/// the Wayland receive pipe; log refusal, preserve clipboard ownership and let
/// cooperative clients retry. Never describe the refused pipe as successful data.
pub(super) fn send(pipe: WritePipe, data: Arc<Vec<u8>>) {
    if let Err(err) = enqueue(pipe, data) {
        log::warn!("native clipboard transfer was not admitted: {err}");
    }
}

fn enqueue(pipe: WritePipe, data: Arc<Vec<u8>>) -> anyhow::Result<()> {
    if PIPES
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            (n < MAX_PIPES).then_some(n + 1)
        })
        .is_err()
    {
        anyhow::bail!("pipe capacity exceeded");
    }
    let mut permit = Permit { bytes: 0 };
    if BYTES
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(data.len()).filter(|n| *n <= MAX_BYTES)
        })
        .is_err()
    {
        anyhow::bail!("retained byte capacity exceeded");
    }
    permit.bytes = data.len();
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let task = PendingSend {
        pipe,
        data,
        offset: 0,
        deadline: Instant::now() + Duration::from_secs(10),
        _permit: permit,
    };
    let sender = SENDER.get_or_init(|| {
        let (tx, rx) = mpsc::sync_channel(MAX_PIPES);
        std::thread::spawn(move || run(rx));
        tx
    });
    sender
        .try_send(task)
        .map_err(|_| anyhow::anyhow!("native transfer queue unavailable"))
}

fn run(receiver: mpsc::Receiver<PendingSend>) {
    let mut pending = Vec::<PendingSend>::new();
    loop {
        if pending.is_empty() {
            match receiver.recv() {
                Ok(task) => pending.push(task),
                Err(_) => return,
            }
        }
        pending.extend(receiver.try_iter());
        let mut fds: Vec<_> = pending
            .iter()
            .map(|task| libc::pollfd {
                fd: task.pipe.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            })
            .collect();
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 100) };
        if ready < 0 {
            if std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
                log::warn!("native clipboard sender poll failed");
                pending.clear();
            }
            continue;
        }
        let now = Instant::now();
        let mut index = 0;
        pending.retain_mut(|task| {
            let events = fds[index].revents;
            index += 1;
            if task.offset == task.data.len() {
                return false;
            }
            if now >= task.deadline {
                log::warn!("native clipboard receiver stalled");
                return false;
            }
            if events == 0 {
                return true;
            }
            if events & libc::POLLNVAL != 0 {
                return false;
            }
            // Bound work per ready descriptor so a large format cannot starve
            // another consumer that started reading a later requested format.
            let end = (task.offset + 64 * 1024).min(task.data.len());
            match task.pipe.write(&task.data[task.offset..end]) {
                Ok(0) => false,
                Ok(n) => {
                    task.offset += n;
                    task.deadline = Instant::now() + Duration::from_secs(10);
                    task.offset < task.data.len()
                }
                Err(err)
                    if matches!(
                        err.kind(),
                        std::io::ErrorKind::Interrupted | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    true
                }
                Err(_) => {
                    log::warn!("native clipboard receiver disconnected");
                    false
                }
            }
        });
    }
}

#[cfg(test)]
mod tests;
