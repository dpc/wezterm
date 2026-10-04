use super::*;
use crate::clipboard::ClipboardSession;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Mutex};
use std::time::Duration;

struct Sink {
    bytes: Arc<Mutex<Vec<u8>>>,
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
    flushed: mpsc::Sender<usize>,
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
            self.release.recv().unwrap();
        }
        let n = bytes.len().min(3);
        self.bytes.lock().unwrap().extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.flushed.send(self.bytes.lock().unwrap().len()).ok();
        Ok(())
    }
}

#[derive(Debug)]
struct Config;
impl TerminalConfiguration for Config {
    fn color_palette(&self) -> ColorPalette {
        ColorPalette::default()
    }
}

fn wait_for(rx: &mpsc::Receiver<usize>, length: usize) {
    loop {
        if rx.recv_timeout(Duration::from_secs(2)).unwrap() >= length {
            break;
        }
    }
}

#[test]
fn gui_enqueue_flushes_prior_bytes_and_fifo_orders_protocol_and_probe() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (started, start) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let sink = Sink {
        bytes: bytes.clone(),
        started: Some(started),
        release: gate,
        flushed,
    };
    let mut term = TerminalState::new(
        TerminalSize::default(),
        Arc::new(Config),
        "test",
        "0",
        Box::new(sink),
    );
    term.writer.write_all(b"buffered").unwrap();
    term.enqueue_input(b"gui").unwrap();
    start.recv_timeout(Duration::from_secs(2)).unwrap();
    let s = ClipboardSession::new(Arc::new(AtomicU64::new(0)));
    let mut response = s.response("read", "x".into(), None);
    response
        .data
        .insert("text/plain".into(), Arc::new(b"hi".to_vec()));
    let mut expected = b"bufferedgui".to_vec();
    response.write_to(&mut expected).unwrap();
    term.send_clipboard_response(response);
    term.decqrm_response(
        Mode::QueryDecPrivateMode(DecPrivateMode::Unspecified(5522)),
        true,
        false,
    );
    term.enqueue_input(b"after").unwrap();
    term.writer.get_mut().flush().unwrap();
    expected.extend_from_slice(b"\x1b[?5522;2$yafter");
    release.send(()).unwrap();
    wait_for(&flush, expected.len());
    assert_eq!(*bytes.lock().unwrap(), expected);
}

#[test]
fn revoke_started_frame_then_probe_never_splices_or_sends_done() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (started, start) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let mut writer = ThreadedWriter::new(Box::new(Sink {
        bytes: bytes.clone(),
        started: Some(started),
        release: gate,
        flushed,
    }));
    let generation = Arc::new(AtomicU64::new(0));
    let s = ClipboardSession::new(generation.clone());
    let mut response = s.response("read", "old".into(), None);
    response
        .data
        .insert("text/plain".into(), Arc::new(b"secret".to_vec()));
    writer.clipboard(response);
    start.recv_timeout(Duration::from_secs(2)).unwrap();
    generation.fetch_add(1, Ordering::AcqRel);
    writer.write_all(b"\x1b[?5522;2$y").unwrap();
    writer.flush().unwrap();
    release.send(()).unwrap();
    let expected = b"\x1b]5522;type=read:id=old:status=OK\x1b\\\x1b[?5522;2$y";
    wait_for(&flush, expected.len());
    assert_eq!(*bytes.lock().unwrap(), expected);
}

#[test]
fn old_async_completion_enqueued_after_fence_done_emits_no_bytes() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (_release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let mut writer = ThreadedWriter::new(Box::new(Sink {
        bytes: bytes.clone(),
        started: None,
        release: gate,
        flushed,
    }));
    let generation = Arc::new(AtomicU64::new(0));
    let mut s = ClipboardSession::new(generation.clone());
    let original_epoch = generation.load(Ordering::Acquire);
    generation.fetch_add(1, Ordering::AcqRel);
    s.expire(std::time::Instant::now());
    let mut fence = s.response("read", "fresh-fence".into(), None);
    fence
        .data
        .insert(".".into(), Arc::new(b"text/plain".to_vec()));
    let mut expected = vec![];
    fence.write_to(&mut expected).unwrap();
    writer.clipboard(fence);
    wait_for(&flush, expected.len());
    // Model a native worker finishing only after the correlated fence is sent.
    let mut late = s.response("read", "old".into(), None);
    late.epoch = original_epoch;
    late.data
        .insert("image/png".into(), Arc::new(b"must not leak".to_vec()));
    writer.clipboard(late);
    writer.write_all(b"barrier").unwrap();
    writer.flush().unwrap();
    expected.extend_from_slice(b"barrier");
    wait_for(&flush, expected.len());
    assert_eq!(*bytes.lock().unwrap(), expected);
}

#[test]
fn active_response_retains_byte_budget_until_sink_completes() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (started, start) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let mut writer = ThreadedWriter::new(Box::new(Sink {
        bytes: bytes.clone(),
        started: Some(started),
        release: gate,
        flushed,
    }));
    writer.clipboard_limit = 4;
    let s = ClipboardSession::new(Arc::new(AtomicU64::new(0)));
    let mut response = s.response("read", "first".into(), None);
    response
        .data
        .insert("text/plain".into(), Arc::new(b"data".to_vec()));
    writer.clipboard(response);
    start.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(writer.clipboard_bytes.load(Ordering::Acquire), 4);
    let mut response = s.response("read", "next".into(), None);
    response
        .data
        .insert("text/plain".into(), Arc::new(b"x".to_vec()));
    writer.clipboard(response);
    assert_eq!(writer.clipboard_bytes.load(Ordering::Acquire), 4);
    writer.write_all(b"barrier").unwrap();
    writer.flush().unwrap();
    release.send(()).unwrap();
    loop {
        flush.recv_timeout(Duration::from_secs(2)).unwrap();
        if bytes.lock().unwrap().ends_with(b"barrier") {
            break;
        }
    }
    assert_eq!(writer.clipboard_bytes.load(Ordering::Acquire), 0);
    let wire = String::from_utf8(bytes.lock().unwrap().clone()).unwrap();
    assert!(wire.contains("id=next:status=EBUSY"));
    assert!(!wire.contains("id=next:status=OK"));
}

#[test]
fn saturated_revoked_queue_rejects_gesture_reservation_before_replacing_grant() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (started, start) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let mut writer = ThreadedWriter::new(Box::new(Sink {
        bytes: bytes.clone(),
        started: Some(started),
        release: gate,
        flushed,
    }));
    let generation = Arc::new(AtomicU64::new(0));
    let mut s = ClipboardSession::new(generation.clone());
    writer.clipboard(s.response("read", "started".into(), Some("EPERM")));
    start.recv_timeout(Duration::from_secs(2)).unwrap();
    for _ in 0..63 {
        generation.fetch_add(1, Ordering::AcqRel);
        s.expire(std::time::Instant::now());
        writer.clipboard(s.response("read", "stale".into(), Some("EPERM")));
    }
    drop(
        s.offer(
            "b2xk".into(),
            vec!["text/plain".into()],
            false,
            std::time::Instant::now(),
        )
        .unwrap(),
    );
    assert!(writer.reserve_clipboard_response().is_none());
    assert!(
        s.has_grant(),
        "rejected gesture must not mutate existing grant"
    );
    generation.fetch_add(1, Ordering::AcqRel);
    writer.write_all(b"barrier").unwrap();
    writer.flush().unwrap();
    release.send(()).unwrap();
    loop {
        flush.recv_timeout(Duration::from_secs(2)).unwrap();
        if bytes.lock().unwrap().ends_with(b"barrier") {
            break;
        }
    }
    assert_eq!(writer.clipboard_pending.load(Ordering::Acquire), 0);
    assert!(writer.reserve_clipboard_response().is_some());
}

struct Attachment {
    attached: std::sync::atomic::AtomicBool,
    enabled: std::sync::atomic::AtomicBool,
    epoch: AtomicU64,
}
impl Attachment {
    fn new() -> Self {
        Self {
            attached: std::sync::atomic::AtomicBool::new(true),
            enabled: std::sync::atomic::AtomicBool::new(false),
            epoch: AtomicU64::new(0),
        }
    }
}
impl crate::clipboard::ClipboardHandler for Attachment {
    fn supported(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }
    fn detach(&self) {
        self.attached.store(false, Ordering::Release);
        self.set_enabled(false);
    }
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    fn set_enabled(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::Release);
        if !enabled {
            self.invalidate();
        }
    }
    fn invalidate(&self) {
        self.epoch.fetch_add(1, Ordering::AcqRel);
    }
    fn handle(&self, _: Vec<Vec<u8>>) {}
}

#[test]
fn reset_replacement_and_detach_revoke_without_clearing_new_attachment() {
    use crate::clipboard::ClipboardHandler;
    let bytes = Arc::new(Mutex::new(vec![]));
    let (_release, gate) = mpsc::channel();
    let (flushed, flush) = mpsc::channel();
    let mut term = Terminal::new(
        TerminalSize::default(),
        Arc::new(Config),
        "test",
        "0",
        Box::new(Sink {
            bytes: bytes.clone(),
            started: None,
            release: gate,
            flushed,
        }),
    );
    let old = Arc::new(Attachment::new());
    term.set_mime_clipboard(old.clone());
    term.advance_bytes(b"\x1b[?5522h");
    assert!(old.enabled());
    term.advance_bytes(b"\x1b[!p");
    assert_eq!(old.epoch.load(Ordering::Acquire), 1);
    assert!(
        old.enabled(),
        "DECSTR revokes authority, retaining selected mode"
    );
    term.advance_bytes(b"\x1bc");
    assert!(!old.enabled());
    let current = Arc::new(Attachment::new());
    term.set_mime_clipboard(current.clone());
    assert!(!old.supported());
    old.detach();
    assert!(
        current.supported(),
        "old window teardown cannot clear replacement"
    );
    current.detach();
    term.advance_bytes(b"\x1b[?5522$p");
    term.writer.get_mut().flush().unwrap();
    wait_for(&flush, b"\x1b[?5522;0$y".len());
    assert!(bytes.lock().unwrap().ends_with(b"\x1b[?5522;0$y"));
}
