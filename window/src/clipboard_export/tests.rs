use super::*;

fn bundle(name: &str, mime: &str, body: &[u8]) -> Vec<u8> {
    let mut bytes = b"TAUFILES\0".to_vec();
    bytes.extend(1u32.to_be_bytes());
    bytes.extend((name.len() as u16).to_be_bytes());
    bytes.extend((mime.len() as u16).to_be_bytes());
    bytes.extend((body.len() as u64).to_be_bytes());
    bytes.extend(name.as_bytes());
    bytes.extend(mime.as_bytes());
    bytes.extend(body);
    bytes
}

#[test]
fn exact_framing_safe_names_and_full_size_body() {
    let body = vec![0xa5; FILE_BODY_LIMIT];
    let bytes = bundle(
        "opaque \u{1f980} #%?.bin",
        "application/octet-stream",
        &body,
    );
    let (name, decoded) = parse(&bytes).unwrap();
    assert_eq!(decoded, body);
    assert_eq!(name, "opaque \u{1f980} #%?.bin");
    let max = bundle(
        &"n".repeat(255),
        &format!("{}/{}", "a".repeat(127), "b".repeat(127)),
        &body,
    );
    assert_eq!(max.len(), FILE_BODY_LIMIT + 535);
    assert_eq!(parse(&max).unwrap().1.len(), FILE_BODY_LIMIT);
    for name in ["", ".", "..", "../x", "a/b", "a\\b", "a\nb", "a\0b"] {
        assert!(parse(&bundle(name, "", b"")).is_err(), "{:?}", name);
    }
    assert!(parse(&bundle(&"x".repeat(256), "", b"")).is_err());
    for mime in [
        "text",
        "text/",
        "/plain",
        "text/plain;a=b",
        "text/\u{e9}",
        "a/b/c",
    ] {
        assert!(parse(&bundle("x", mime, b"")).is_err(), "{:?}", mime);
    }
    let mut extra = bundle("x", "", b"");
    extra.push(0);
    assert!(parse(&extra).is_err());
    assert!(parse(&bytes[..bytes.len() - 1]).is_err());
    assert!(parse(&bundle("x", "", &vec![0; FILE_BODY_LIMIT + 1])).is_err());
    let mut count = bundle("x", "", b"");
    count[12] = 2;
    assert!(parse(&count).is_err());
}

#[test]
fn copied_file_metadata_resolves_only_local_opaque_body_and_rolls_back() {
    let root = tempfile::tempdir().unwrap();
    let bytes = b"\0opaque\xffbody\r\n";
    let prepared = stage(root.path(), "snow \u{2603} #%?.bin", bytes, &|| Ok(())).unwrap();
    let uri_list = std::str::from_utf8(&prepared.data["text/uri-list"]).unwrap();
    assert!(uri_list.ends_with("\r\n"));
    let uri = uri_list.trim_end();
    assert!(uri.contains("%23%25%3F"));
    assert_eq!(
        prepared.data["x-special/gnome-copied-files"].as_slice(),
        format!("copy\n{uri}").as_bytes()
    );
    assert_eq!(
        prepared.data["application/x-kde-cutselection"].as_slice(),
        b"0"
    );
    assert_eq!(prepared.data.len(), 3);
    let path = url::Url::parse(uri).unwrap().to_file_path().unwrap();
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    assert_eq!(
        fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
        0o700
    );
    drop(prepared);
    assert!(!path.exists());
}

#[test]
fn retention_quota_counts_crashed_entries_and_never_evicts_live_paths() {
    let root = tempfile::tempdir().unwrap();
    let first = stage(root.path(), "x", b"first", &|| Ok(())).unwrap();
    let first_path = first.directory.path().join("x");
    first.retain(); // Also models a crashed process's committed cache entry.
    for _ in 1..MAX_EXPORTS {
        stage(root.path(), "x", b"", &|| Ok(())).unwrap().retain();
    }
    let error = stage(root.path(), "x", b"", &|| Ok(())).err().unwrap();
    assert!(matches!(
        error.downcast_ref(),
        Some(ClipboardError::Temporary)
    ));
    assert_eq!(fs::read(&first_path).unwrap(), b"first");
    assert_eq!(reap(root.path(), SystemTime::now()).unwrap(), MAX_EXPORTS);
    assert_eq!(reap(root.path(), SystemTime::now() + LIFETIME).unwrap(), 0);
    assert!(!first_path.exists());
}

#[test]
fn cancellation_removes_partial_export_and_lock_contention_is_bounded() {
    let root = tempfile::tempdir().unwrap();
    let lock = lock(root.path()).unwrap();
    assert!(stage(root.path(), "x", b"body", &|| Ok(())).is_err());
    drop(lock);
    let checks = std::cell::Cell::new(0);
    assert!(stage(root.path(), "x", &[0; 128 * 1024], &|| {
        checks.set(checks.get() + 1);
        if checks.get() == 3 {
            anyhow::bail!("cancelled");
        }
        Ok(())
    })
    .is_err());
    assert_eq!(reap(root.path(), SystemTime::now()).unwrap(), 0);
}

#[test]
fn expired_preparation_is_not_reaped_until_gui_rejects_it() {
    let root = tempfile::tempdir().unwrap();
    let mut prepared = stage(root.path(), "x", b"body", &|| Ok(())).unwrap();
    let path = prepared.directory.path().join("x");
    prepared.expires = Instant::now();
    assert!(prepared.expired());
    assert_eq!(reap(root.path(), SystemTime::now() + LIFETIME).unwrap(), 1);
    assert_eq!(fs::read(&path).unwrap(), b"body");
    drop(prepared);
    assert!(!path.exists());
}

#[test]
fn runtime_paths_reject_symlinks_and_shared_permissions() {
    let temp = tempfile::tempdir().unwrap();
    let runtime = temp.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(validated_root(runtime.clone()).is_err());
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&runtime, &link).unwrap();
    assert!(validated_root(link).is_err());
    let root = runtime.join("wezterm-file-export-v1");
    std::os::unix::fs::symlink(temp.path(), &root).unwrap();
    assert!(validated_root(runtime.clone()).is_err());
    fs::remove_file(&root).unwrap();
    assert_eq!(validated_root(runtime).unwrap(), root);
    assert_eq!(fs::metadata(root).unwrap().mode() & 0o777, 0o700);
}
