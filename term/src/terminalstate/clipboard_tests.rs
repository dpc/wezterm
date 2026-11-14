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
    epoch: Arc<AtomicU64>,
}
impl Attachment {
    fn new() -> Self {
        Self {
            attached: std::sync::atomic::AtomicBool::new(true),
            enabled: std::sync::atomic::AtomicBool::new(false),
            epoch: Arc::new(AtomicU64::new(0)),
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
fn window_focus_reports_preserve_pending_writes_but_hard_boundaries_cancel() {
    use crate::clipboard::{ClipboardAction, ClipboardHandler};
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use std::time::Instant;

    for file_export in [false, true] {
        for boundary in [
            "window",
            "pane",
            "off",
            "soft-reset",
            "reset",
            "replace",
            "detach",
        ] {
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
            let attachment = Arc::new(Attachment::new());
            term.set_mime_clipboard(attachment.clone());
            term.advance_bytes(b"\x1b[?5522h\x1b[?1004h");
            term.window_focus_changed(true);
            let mut session = ClipboardSession::new(attachment.epoch.clone());
            let kind = if file_export { "write-file" } else { "write" };
            let continuation = if file_export {
                "write-file-data"
            } else {
                "wdata"
            };
            let mime = STANDARD.encode(if file_export {
                "application/vnd.tau.file-export-v1"
            } else {
                "text/plain"
            });
            let handle = |session: &mut ClipboardSession, fields: Vec<Vec<u8>>| {
                session.handle(&fields, false, true, Instant::now())
            };
            assert!(handle(&mut session, vec![format!("type={kind}:id=w").into_bytes()]).is_none());
            assert!(handle(
                &mut session,
                vec![
                    format!("type={continuation}:id=w:mime={mime}").into_bytes(),
                    STANDARD.encode(b"accepted before blur").into_bytes(),
                ],
            )
            .is_none());

            // A callback may repeat, and a pane handoff can arrive while the
            // window is already unfocused. Neither case may confuse the epoch.
            term.window_focus_changed(false);
            term.window_focus_changed(false);
            match boundary {
                "window" => {}
                "pane" => term.focus_changed(false),
                "off" => term.advance_bytes(b"\x1b[?5522l"),
                "soft-reset" => term.advance_bytes(b"\x1b[!p"),
                "reset" => term.advance_bytes(b"\x1bc"),
                "replace" => term.set_mime_clipboard(Arc::new(Attachment::new())),
                "detach" => attachment.detach(),
                _ => unreachable!(),
            }
            let action = handle(
                &mut session,
                vec![format!("type={continuation}:id=w").into_bytes()],
            );
            if boundary == "window" {
                match action {
                    Some(ClipboardAction::Write { data, .. }) => {
                        assert_eq!(
                            data.values().next().unwrap().as_slice(),
                            b"accepted before blur"
                        );
                    }
                    _ => panic!("window blur canceled {}", kind),
                }
                // An async completion retains its original epoch through both
                // blur and refocus, including its eventual DONE response.
                let reply = session.response(kind, "w".into(), None);
                term.window_focus_changed(true);
                term.window_focus_changed(true);
                let mut output = vec![];
                reply.write_to(&mut output).unwrap();
                assert_eq!(
                    output,
                    format!("\x1b]5522;type={kind}:id=w:status=DONE\x1b\\").as_bytes()
                );
                term.advance_bytes(b"\x1b[?5522$p");
                loop {
                    flush.recv_timeout(Duration::from_secs(2)).unwrap();
                    if bytes.lock().unwrap().ends_with(b"\x1b[?5522;1$y") {
                        break;
                    }
                }
                assert!(bytes
                    .lock()
                    .unwrap()
                    .ends_with(b"\x1b[O\x1b[I\x1b[?5522;1$y"));
            } else {
                assert!(
                    action.is_none(),
                    "{} must discard the partial write",
                    boundary
                );
            }
        }
    }
}

#[test]
fn accepted_read_grant_and_response_survive_window_focus_not_pane_handoff() {
    use crate::clipboard::ClipboardAction;
    use std::time::Instant;

    let (_release, gate) = mpsc::channel();
    let (flushed, _flush) = mpsc::channel();
    let mut term = Terminal::new(
        TerminalSize::default(),
        Arc::new(Config),
        "test",
        "0",
        Box::new(Sink {
            bytes: Arc::new(Mutex::new(vec![])),
            started: None,
            release: gate,
            flushed,
        }),
    );
    let attachment = Arc::new(Attachment::new());
    term.set_mime_clipboard(attachment.clone());
    let mut session = ClipboardSession::new(attachment.epoch.clone());
    drop(
        session
            .offer(
                "c2VjcmV0".into(),
                vec!["text/plain".into()],
                false,
                Instant::now(),
            )
            .unwrap(),
    );
    term.window_focus_changed(false);
    let action = session.handle(
        &[
            b"type=read:pw=c2VjcmV0:name=YXBw".to_vec(),
            b"dGV4dC9wbGFpbg==".to_vec(),
        ],
        false,
        false,
        Instant::now(),
    );
    assert!(matches!(
        action,
        Some(ClipboardAction::Read { retained: true, .. })
    ));
    assert!(
        !session.has_grant(),
        "the accepted read still consumes its one-use grant"
    );
    let reply = session.response("read", "r".into(), None);
    let mut output = vec![];
    reply.write_to(&mut output).unwrap();
    assert!(output.ends_with(b"status=DONE\x1b\\"));
    term.focus_changed(false);
    output.clear();
    reply.write_to(&mut output).unwrap();
    assert!(
        output.is_empty(),
        "actual pane handoff cancels the old read response"
    );
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
