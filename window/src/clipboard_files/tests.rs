use super::*;
use std::io::Write;
use std::os::unix::fs::symlink;
use std::sync::atomic::{AtomicUsize, Ordering};

fn temp_dir() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let path = std::env::temp_dir().join(format!(
        "wezterm-file-tests-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir(&path).unwrap();
    path
}

#[test]
fn uri_metadata_is_not_file_body_and_rejects_nonlocal_authority() {
    let paths = parse_paths(b"# comment\r\nfile:///tmp/a%20b\r\n", URI_LIST)
        .unwrap()
        .unwrap();
    assert_eq!(paths, [PathBuf::from("/tmp/a b")]);
    assert_eq!(
        parse_paths(b"cut\nfile:///tmp/a\n", GNOME_FILES)
            .unwrap()
            .unwrap(),
        [PathBuf::from("/tmp/a")]
    );
    for bytes in [
        &b"file://host/tmp/file"[..],
        &b"file:///tmp/a%00b"[..],
        &b"file:///tmp/a%Q0"[..],
        &b"file:///tmp/a?query"[..],
        &b"file:///tmp/a#fragment"[..],
        &b"\xff"[..],
        &b""[..],
    ] {
        assert!(parse_paths(bytes, URI_LIST).is_err(), "{:?}", bytes);
    }
    assert!(parse_paths(b"move\nfile:///tmp/a", GNOME_FILES).is_err());
    assert!(parse_paths(b"https://example.com\nfile:///tmp/a\n", URI_LIST).is_err());
    assert!(parse_paths(b"copy\nhttps://example.com\n", GNOME_FILES).is_err());
}

#[test]
fn opaque_bodies_symlinks_duplicates_and_empty_files_have_exact_framing() {
    let dir = temp_dir();
    let file = dir.join("test.mp3");
    std::fs::write(&file, [0, 255, 27, 10]).unwrap();
    let link = dir.join("linked");
    symlink(&file, &link).unwrap();
    let empty = dir.join("empty");
    std::fs::write(&empty, []).unwrap();
    let bytes = build_bundle(&[link, file.clone(), file.clone(), empty], 1024, &|| Ok(())).unwrap();
    let mut expected = b"TAUFILES\0".to_vec();
    expected.extend_from_slice(&4u32.to_be_bytes());
    for (name, body) in [
        ("linked", &[0, 255, 27, 10][..]),
        ("test.mp3", &[0, 255, 27, 10]),
        ("test.mp3", &[0, 255, 27, 10]),
        ("empty", &[]),
    ] {
        expected.extend_from_slice(&(name.len() as u16).to_be_bytes());
        expected.extend_from_slice(&0u16.to_be_bytes());
        expected.extend_from_slice(&(body.len() as u64).to_be_bytes());
        expected.extend_from_slice(name.as_bytes());
        expected.extend_from_slice(body);
    }
    assert_eq!(bytes, expected);
    assert_eq!(
        build_bundle(std::slice::from_ref(&file), 13 + 12 + 8 + 4, &|| Ok(()))
            .unwrap()
            .len(),
        37
    );
    assert!(build_bundle(&[file], 36, &|| Ok(())).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn no_discretionary_file_count_limit_and_bounded_cancellation() {
    let dir = temp_dir();
    let file = dir.join("a");
    std::fs::write(&file, []).unwrap();
    assert!(build_bundle(&vec![file; 32], 13 + 32 * 13, &|| Ok(())).is_ok());
    assert!(build_bundle(std::slice::from_ref(&dir), 1024, &|| Ok(())).is_err());
    let fifo = dir.join("fifo");
    let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    assert!(build_bundle(&[fifo], 1024, &|| Ok(())).is_err());
    let file = dir.join("a");
    assert!(build_bundle(&[file], 1024, &|| Err(ClipboardError::Stale.into())).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_basename_is_never_transport_metadata() {
    let dir = temp_dir();
    let file = dir.join("evil\nname");
    std::fs::File::create(&file)
        .unwrap()
        .write_all(b"opaque")
        .unwrap();
    assert!(build_bundle(&[file], 1024, &|| Ok(())).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

struct EmptyOffer(Vec<String>);

impl ClipboardOffer for EmptyOffer {
    fn mime_types(&self) -> &[String] {
        &self.0
    }
    fn valid(&self) -> bool {
        true
    }
    fn read(
        &self,
        _: Vec<String>,
        _: usize,
        _: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Future<ClipboardData> {
        Future::ok(ClipboardData::new())
    }
}

#[test]
fn queued_inventory_validity_does_not_retain_snapshot_bodies() {
    let bundle = Arc::new(vec![0; 8192]);
    let weak = Arc::downgrade(&bundle);
    let offer = FileOffer {
        native: Arc::new(EmptyOffer(vec![])),
        types: vec![FILE_BUNDLE_MIME.to_owned()],
        bundle,
    };
    let validity = offer.validity_check().unwrap();
    drop(offer);
    assert!(weak.upgrade().is_none());
    assert!(validity());
}

#[test]
fn retained_capacity_matches_response_accounting_at_and_below_cap() {
    let dir = temp_dir();
    let file = dir.join("a");
    for length in [0, 8192, 16 * 1024 * 1024 - 26] {
        std::fs::File::create(&file)
            .unwrap()
            .set_len(length)
            .unwrap();
        let bytes =
            build_bundle(std::slice::from_ref(&file), FILE_BUNDLE_LIMIT, &|| Ok(())).unwrap();
        assert_eq!(bytes.capacity(), bytes.len());
        assert!(bytes.capacity() <= FILE_BUNDLE_LIMIT);
    }
    let mut backing = Vec::new();
    for _ in 0..99 {
        append(&mut backing, &[0; 11], 1090).unwrap();
        assert!(backing.capacity() <= 1090);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ordinary_browser_uri_clipboard_preserves_native_offer_without_file_mime() {
    let native: Arc<dyn ClipboardOffer> = Arc::new(EmptyOffer(vec![
        URI_LIST.to_owned(),
        "text/plain;charset=utf-8".to_owned(),
    ]));
    let result = finish_snapshot(
        native.clone(),
        b"https://example.com/a\r\nhttp://example.com/b\r\n",
        URI_LIST,
        1024,
        &|| Ok(()),
    )
    .unwrap();
    assert!(Arc::ptr_eq(&native, &result));
    assert!(!result.mime_types().iter().any(|m| m == FILE_BUNDLE_MIME));
    assert!(
        finish_snapshot(native, b"https://example.com/%QZ", URI_LIST, 1024, &|| Ok(
            ()
        ))
        .is_err()
    );
}
