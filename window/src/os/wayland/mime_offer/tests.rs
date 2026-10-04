use super::*;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

fn pipe(data: &[u8]) -> ReadPipe {
    let (reader, mut writer) = UnixStream::pair().unwrap();
    writer.write_all(data).unwrap();
    drop(writer);
    let fd: OwnedFd = reader.into();
    ReadPipe::from(fd)
}

#[test]
fn retained_offer_has_all_mime_names_and_is_revoked_on_owner_change() {
    let generation = Arc::new(AtomicU64::new(0));
    let offer = MimeOffer {
        native: None,
        types: vec![
            "image/png".into(),
            "text/uri-list".into(),
            "application/custom".into(),
        ],
        generation: generation.clone(),
        epoch: 0,
    };
    assert_eq!(
        offer.mime_types(),
        ["image/png", "text/uri-list", "application/custom"]
    );
    assert!(offer.valid());
    generation.fetch_add(1, Ordering::AcqRel);
    assert!(!offer.valid());
}

#[test]
fn native_binary_pipe_preserves_bytes_and_enforces_exact_limit() {
    let bytes = [0, 255, 13, 10, 27, 0, 128];
    let generation = AtomicU64::new(0);
    let deadline = Instant::now() + Duration::from_secs(2);
    assert_eq!(
        read_bounded(pipe(&bytes), bytes.len(), deadline, &generation, 0, &|| {
            true
        })
        .unwrap(),
        bytes
    );
    let err = read_bounded(
        pipe(&bytes),
        bytes.len() - 1,
        deadline,
        &generation,
        0,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<ClipboardError>(),
        Some(ClipboardError::Limit)
    ));
    assert!(
        read_bounded(pipe(&[]), 0, deadline, &generation, 0, &|| true)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn native_read_timeout_and_app_generation_cancel_are_bounded() {
    let generation = Arc::new(AtomicU64::new(0));
    let err = read_bounded(
        pipe(b"not read"),
        64,
        Instant::now(),
        &generation,
        0,
        &|| true,
    )
    .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<ClipboardError>(),
        Some(ClipboardError::Temporary)
    ));
    let (reader, _writer) = UnixStream::pair().unwrap();
    let fd: OwnedFd = reader.into();
    let other = generation.clone();
    let (started, start) = mpsc::channel();
    let (done, result) = mpsc::channel();
    std::thread::spawn(move || {
        started.send(()).unwrap();
        done.send(read_bounded(
            ReadPipe::from(fd),
            64,
            Instant::now() + Duration::from_secs(10),
            &other,
            0,
            &|| true,
        ))
        .unwrap();
    });
    start.recv_timeout(Duration::from_secs(1)).unwrap();
    generation.fetch_add(1, Ordering::AcqRel);
    let err = result
        .recv_timeout(Duration::from_secs(1))
        .unwrap()
        .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<ClipboardError>(),
        Some(ClipboardError::Stale)
    ));
}
