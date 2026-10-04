use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

struct Gate {
    bytes: Arc<Mutex<Vec<u8>>>,
    started: Option<mpsc::Sender<()>>,
    release: mpsc::Receiver<()>,
}
impl Write for Gate {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(started) = self.started.take() {
            started.send(()).unwrap();
            self.release.recv().unwrap();
        }
        let n = bytes.len().min(3);
        self.bytes.lock().extend_from_slice(&bytes[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn synchronous_input_cannot_splice_short_protocol_frame() {
    let bytes = Arc::new(Mutex::new(vec![]));
    let (started, start) = mpsc::channel();
    let (release, gate) = mpsc::channel();
    let mut protocol = WriterWrapper::new(Box::new(Gate {
        bytes: bytes.clone(),
        started: Some(started),
        release: gate,
    }));
    let mut ordinary = protocol.clone();
    let frame = b"\x1b]5522;type=read:id=x:status=OK\x1b\\";
    let a = std::thread::spawn(move || protocol.write_all(frame).unwrap());
    start.recv_timeout(Duration::from_secs(2)).unwrap();
    let (done, completed) = mpsc::channel();
    let b = std::thread::spawn(move || {
        ordinary.write_all(b"KEY").unwrap();
        done.send(()).unwrap();
    });
    assert!(completed.recv_timeout(Duration::from_millis(100)).is_err());
    release.send(()).unwrap();
    a.join().unwrap();
    b.join().unwrap();
    let mut expected = frame.to_vec();
    expected.extend_from_slice(b"KEY");
    assert_eq!(*bytes.lock(), expected);
}

#[test]
fn fatal_error_latches_all_clones_and_notifies_after_unlock_once() {
    struct Fail;
    impl Write for Fail {
        fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
            Err(std::io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let count = Arc::new(AtomicUsize::new(0));
    let mut writer = WriterWrapper::new(Box::new(Fail));
    let shared = writer.writer.clone();
    let notice_count = count.clone();
    writer.failure_notice = Some(Arc::new(move || {
        assert!(
            shared.try_lock().is_some(),
            "notice must not hold transport mutex"
        );
        notice_count.fetch_add(1, Ordering::Relaxed);
    }));
    let mut clone = writer.clone();
    assert!(writer.write_all(b"frame").is_err());
    assert!(clone.write_all(b"must not append").is_err());
    assert!(clone.flush().is_err());
    assert_eq!(count.load(Ordering::Relaxed), 1);
}
