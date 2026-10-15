use super::*;

#[test]
fn file_export_has_dedicated_family_and_raw_mime_stays_opaque() {
    let mut s = session();
    let mime = STANDARD.encode("application/vnd.tau.file-export-v1");
    assert!(handle(&mut s, "type=write-file:id=file", None).is_none());
    assert_eq!(status(handle(&mut s, "type=wdata:id=file", None)), "EBUSY");
    assert_eq!(
        status(handle(&mut s, "type=write-file-data:id=other", None)),
        "EBUSY"
    );
    assert_eq!(
        status(handle(&mut s, "type=write-file-data", None)),
        "EINVAL"
    );
    assert!(handle(
        &mut s,
        &format!("type=write-file-data:id=file:mime={mime}"),
        Some("AAE="),
    )
    .is_none());
    match handle(&mut s, "type=write-file-data:id=file", None) {
        Some(ClipboardAction::Write {
            file_export, data, ..
        }) => {
            assert!(file_export);
            assert_eq!(
                data["application/vnd.tau.file-export-v1"].as_slice(),
                &[0, 1]
            );
        }
        _ => panic!("missing file operation"),
    }
    let mut s = session();
    handle(&mut s, "type=write:id=raw", None);
    assert_eq!(
        status(handle(&mut s, "type=write-file-data:id=raw", None)),
        "EBUSY"
    );
    handle(
        &mut s,
        &format!("type=wdata:id=raw:mime={mime}"),
        Some("AAE="),
    );
    match handle(&mut s, "type=wdata:id=raw", None) {
        Some(ClipboardAction::Write {
            file_export, data, ..
        }) => {
            assert!(!file_export);
            assert_eq!(
                data["application/vnd.tau.file-export-v1"].as_slice(),
                &[0, 1]
            );
        }
        _ => panic!("missing raw operation"),
    }
}

#[test]
fn file_export_rejects_unknown_keys_aliases_bad_ids_and_drains_failures() {
    let mut s = session();
    for meta in [
        "type=write-file",
        "type=write-file:id=a!",
        "type=write-file:id=f:loc=primary",
        "type=write-file:id=f:name=eA==",
        "type=write-file:id=f:extra=1",
        "type=write-file:id=f:id=f",
    ] {
        assert_eq!(status(handle(&mut s, meta, None)), "EINVAL", "{meta}");
    }
    handle(&mut s, "type=write-file:id=f", None);
    assert_eq!(
        status(handle(&mut s, "type=walias:id=f", Some("eA=="))),
        "EBUSY"
    );
    let mime = STANDARD.encode("application/vnd.tau.file-export-v1");
    handle(
        &mut s,
        &format!("type=write-file-data:id=f:mime={mime}"),
        Some("eA=="),
    );
    assert_eq!(
        status(handle(
            &mut s,
            "type=write-file-data:id=f:unexpected=x",
            None
        )),
        "EINVAL"
    );
    assert!(handle(&mut s, "type=write-file-data:id=f", None).is_none());
    handle(&mut s, "type=write-file:id=f", None);
    assert_eq!(
        status(handle(&mut s, "type=write-file-data:id=f", Some(""))),
        "EINVAL"
    );
}

#[test]
fn file_export_permission_cancellation_and_dedicated_response() {
    let mut s = session();
    let action = s.handle(
        &packet("type=write-file:id=f", None),
        false,
        false,
        Instant::now(),
    );
    let reply = match action {
        Some(ClipboardAction::Reply(reply)) => reply,
        _ => panic!(),
    };
    let mut wire = vec![];
    reply.write_to(&mut wire).unwrap();
    assert_eq!(wire, b"\x1b]5522;type=write-file:id=f:status=EPERM\x1b\\");
    handle(&mut s, "type=write-file:id=f", None);
    s.generation.fetch_add(1, Ordering::AcqRel);
    assert!(handle(&mut s, "type=write-file-data:id=f", None).is_none());
}

#[test]
fn unsafe_file_ids_never_normalize_into_a_live_transaction() {
    let mut s = session();
    handle(&mut s, "type=write-file:id=f", None);
    for packet in [
        "type=write-file-data:id=f!",
        "type=write-file-data:id=f!:unexpected=x",
        "type=write-file-data:id=f!:id=f!",
        "type=write-file-data",
    ] {
        match handle(&mut s, packet, None) {
            Some(ClipboardAction::Reply(reply)) => {
                assert_eq!(reply.error, Some("EINVAL"));
                assert_eq!(reply.id, "");
            }
            _ => panic!("expected uncorrelated rejection"),
        }
        assert_eq!(s.write.as_ref().unwrap().id, "f");
    }
    let mime = STANDARD.encode("application/vnd.tau.file-export-v1");
    handle(
        &mut s,
        &format!("type=write-file-data:id=f:mime={mime}"),
        Some("eA=="),
    );
    assert!(matches!(
        handle(&mut s, "type=write-file-data:id=f", None),
        Some(ClipboardAction::Write {
            file_export: true,
            ..
        })
    ));
}

#[test]
fn gesture_expiry_deadline_tracks_replacement_and_exact_cleanup() {
    let generation = Arc::new(AtomicU64::new(0));
    let mut session = ClipboardSession::new(generation);
    let now = Instant::now();
    assert_eq!(session.grant_deadline(), None);
    session
        .offer("first".into(), vec!["image/png".into()], false, now)
        .unwrap();
    assert_eq!(session.grant_deadline(), Some(now + TIMEOUT));
    let replacement = now + Duration::from_secs(1);
    session
        .offer(
            "second".into(),
            vec!["image/png".into()],
            false,
            replacement,
        )
        .unwrap();
    session.expire(now + TIMEOUT);
    assert_eq!(session.grant_deadline(), Some(replacement + TIMEOUT));
    session.expire(replacement + TIMEOUT);
    assert_eq!(session.grant_deadline(), None);
    assert!(!session.has_grant());
}

fn session() -> ClipboardSession {
    ClipboardSession::new(Arc::new(AtomicU64::new(0)))
}

fn packet(meta: &str, data: Option<&str>) -> Vec<Vec<u8>> {
    let mut fields = vec![meta.as_bytes().to_vec()];
    if let Some(data) = data {
        fields.push(data.as_bytes().to_vec());
    }
    fields
}

fn handle(s: &mut ClipboardSession, meta: &str, data: Option<&str>) -> Option<ClipboardAction> {
    s.handle(&packet(meta, data), false, true, Instant::now())
}

fn status(action: Option<ClipboardAction>) -> &'static str {
    match action {
        Some(ClipboardAction::Reply(reply)) => reply.error.unwrap(),
        _ => panic!("expected error"),
    }
}

#[test]
fn offer_lists_every_format_and_primary_with_one_time_password() {
    let mut s = session();
    let now = Instant::now();
    let reply = s
        .offer(
            "c2VjcmV0".into(),
            vec![
                "image/png".into(),
                "text/plain".into(),
                "text/uri-list".into(),
            ],
            true,
            now,
        )
        .unwrap();
    let mut wire = vec![];
    reply.write_to(&mut wire).unwrap();
    let wire = String::from_utf8(wire).unwrap();
    assert!(wire.starts_with("\x1b]5522;type=read:status=OK:loc=primary:pw=c2VjcmV0\x1b\\"));
    assert!(wire.contains(&STANDARD.encode("image/png text/plain text/uri-list")));
    assert!(wire.ends_with("\x1b]5522;type=read:status=DONE\x1b\\"));
    assert!(!wire.contains("\x1b[200~"));
}

#[test]
fn grant_permissions_fallback_busy_replay_and_location() {
    let mut s = session();
    let now = Instant::now();
    s.offer("c2VjcmV0".into(), vec!["image/png".into()], false, now)
        .unwrap();
    let png = STANDARD.encode("image/png text/unavailable");
    assert_eq!(
        status(handle(&mut s, "type=read:pw=c2VjcmV0", Some(&png))),
        "EPERM"
    );
    assert!(s.grant.is_some());
    assert_eq!(
        status(handle(
            &mut s,
            "type=read:loc=primary:pw=c2VjcmV0:name=UGFzdGUgZXZlbnQ=",
            Some(&png)
        )),
        "EPERM"
    );
    assert!(s.grant.is_some());
    let ambient = s.handle(
        &packet("type=read:pw=aW52YWxpZA==", Some(&png)),
        true,
        true,
        now,
    );
    match ambient {
        Some(ClipboardAction::Read { retained, busy, .. }) => {
            assert!(!retained);
            busy.store(false, Ordering::Release);
        }
        _ => panic!("configured ambient read must work"),
    }
    assert!(s.grant.is_some());
    let busy = match handle(
        &mut s,
        "type=read:id=test:pw=c2VjcmV0:name=UGFzdGUgZXZlbnQ=",
        Some(&png),
    ) {
        Some(ClipboardAction::Read {
            types,
            retained,
            busy,
            ..
        }) => {
            assert!(retained);
            assert_eq!(types, vec!["image/png"]);
            busy
        }
        _ => panic!("authorized read"),
    };
    assert!(s.grant.is_none());
    assert_eq!(
        status(handle(&mut s, "type=read:id=other", Some(&png))),
        "EBUSY"
    );
    assert!(s
        .offer("bmV3".into(), vec!["text/plain".into()], false, now)
        .is_none());
    busy.store(false, Ordering::Release);
    assert_eq!(
        status(handle(
            &mut s,
            "type=read:pw=c2VjcmV0:name=UGFzdGUgZXZlbnQ=",
            Some(&png)
        )),
        "EPERM"
    );
}

#[test]
fn metadata_listing_does_not_consume_grant() {
    let mut s = session();
    s.offer(
        "c2VjcmV0".into(),
        vec!["text/plain".into()],
        false,
        Instant::now(),
    )
    .unwrap();
    assert!(matches!(
        handle(
            &mut s,
            "type=read:pw=c2VjcmV0:name=UGFzdGUgZXZlbnQ=",
            Some("Lg==")
        ),
        Some(ClipboardAction::List { .. })
    ));
    assert!(s.grant.is_some());
}

#[test]
fn stale_listing_completion_cannot_release_new_generation_busy() {
    let mut s = session();
    let old_busy = match handle(&mut s, "type=read:id=old", Some("Lg==")) {
        Some(ClipboardAction::List { busy, .. }) => busy,
        _ => panic!("listing"),
    };
    s.generation.fetch_add(1, Ordering::AcqRel);
    let new_busy = match handle(&mut s, "type=read:id=new", Some("Lg==")) {
        Some(ClipboardAction::List { busy, .. }) => busy,
        _ => panic!("new listing"),
    };
    let mut stale = s.response("read", "old".into(), None);
    stale.busy = Some(old_busy);
    stale.epoch = 0;
    drop(stale);
    assert!(new_busy.load(Ordering::Acquire));
    assert!(s.is_busy());
}

#[test]
fn stale_gesture_cannot_adopt_generation_after_mode_off_and_fence() {
    let mut s = session();
    let old_epoch = s.generation.load(Ordering::Acquire);
    s.generation.fetch_add(1, Ordering::AcqRel);
    let fence = handle(&mut s, "type=read:id=fresh-fence", Some("Lg=="));
    let busy = match fence {
        Some(ClipboardAction::List { busy, .. }) => busy,
        _ => panic!("fence listing"),
    };
    let mut response = s.response("read", "fresh-fence".into(), None);
    response.busy = Some(busy);
    response
        .data
        .insert(".".into(), Arc::new(b"image/png".to_vec()));
    response.write_to(&mut vec![]).unwrap();
    drop(response);
    assert!(
        !s.is_busy(),
        "epoch check must be the only remaining admission blocker"
    );
    assert!(s
        .offer_for_epoch(
            "c3RhbGU=".into(),
            vec!["image/png".into()],
            false,
            Instant::now(),
            old_epoch
        )
        .is_none());
    assert!(!s.has_grant());
}

#[test]
fn stale_request_cannot_mutate_new_assembly_or_emit_new_epoch_error() {
    let mut s = session();
    s.generation.fetch_add(1, Ordering::AcqRel);
    handle(&mut s, "type=write:id=new", None);
    assert!(s
        .handle_for_epoch(
            &packet("type=wdata:loc=invalid", None),
            false,
            true,
            Instant::now(),
            0
        )
        .is_none());
    assert_eq!(s.write.as_ref().unwrap().id, "new");
    assert!(s
        .handle_for_epoch(
            &packet("type=read", Some("aW1hZ2UvcG5n")),
            false,
            true,
            Instant::now(),
            0
        )
        .is_none());
}

#[test]
fn grant_expiry_replacement_reset_and_no_available_type() {
    let mut s = session();
    let now = Instant::now();
    s.offer("b2xk".into(), vec!["image/png".into()], false, now)
        .unwrap();
    s.offer("bmV3".into(), vec!["text/plain".into()], false, now)
        .unwrap();
    assert_eq!(
        status(handle(
            &mut s,
            "type=read:pw=b2xk:name=UGFzdGUgZXZlbnQ=",
            Some("dGV4dC9wbGFpbg==")
        )),
        "EPERM"
    );
    match handle(
        &mut s,
        "type=read:pw=bmV3:name=UGFzdGUgZXZlbnQ=",
        Some("aW1hZ2UvcG5n"),
    ) {
        Some(ClipboardAction::Read { types, busy, .. }) => {
            assert!(types.is_empty());
            busy.store(false, Ordering::Release);
        }
        _ => panic!("empty available subset"),
    }
    assert!(s.grant.is_some());
    s.expire(now + TIMEOUT);
    assert!(s.grant.is_none());
    s.offer("bmV3".into(), vec!["text/plain".into()], false, now)
        .unwrap();
    s.generation.fetch_add(1, Ordering::AcqRel);
    s.expire(now);
    assert!(s.grant.is_none());
}

#[test]
fn writes_assemble_across_nonbase64_aligned_packets_and_alias_before_data() {
    let mut s = session();
    assert!(handle(&mut s, "type=write:id=mux.1:loc=primary", None).is_none());
    assert!(handle(
        &mut s,
        "type=walias:id=mux.1:mime=dGV4dC9wbGFpbg==",
        Some("dGV4dC94LWFsaWFz")
    )
    .is_none());
    for part in ["a", "GV", "sbG8", "="] {
        assert!(handle(
            &mut s,
            "type=wdata:id=mux.1:mime=dGV4dC9wbGFpbg==",
            Some(part)
        )
        .is_none());
    }
    match handle(&mut s, "type=wdata:id=mux.1", None) {
        Some(ClipboardAction::Write {
            id, primary, data, ..
        }) => {
            assert_eq!(id, "mux.1");
            assert!(primary);
            assert_eq!(data["text/plain"].as_slice(), b"hello");
            assert!(Arc::ptr_eq(&data["text/plain"], &data["text/x-alias"]));
        }
        _ => panic!("atomic write"),
    }
}

#[test]
fn write_errors_drain_then_fresh_write_recovers() {
    let mut s = session();
    handle(&mut s, "type=write:id=x", None);
    assert_eq!(
        status(handle(
            &mut s,
            "type=wdata:mime=dGV4dC9wbGFpbg==",
            Some("!!!!")
        )),
        "EINVAL"
    );
    assert!(handle(&mut s, "type=wdata:mime=dGV4dC9wbGFpbg==", Some("YWJj")).is_none());
    assert!(handle(&mut s, "type=wdata", None).is_none());
    handle(&mut s, "type=write:id=y", None);
    handle(&mut s, "type=wdata:mime=dGV4dC9wbGFpbg==", Some("Y"));
    assert_eq!(status(handle(&mut s, "type=wdata", None)), "EINVAL");
    handle(&mut s, "type=write", None);
    handle(&mut s, "type=wdata:mime=dGV4dC9wbGFpbg==", Some("YQ=="));
    assert_eq!(
        status(handle(
            &mut s,
            "type=wdata:mime=dGV4dC9wbGFpbg==",
            Some("Yg==")
        )),
        "EINVAL"
    );
    handle(&mut s, "type=write", None);
    assert_eq!(
        status(s.handle(
            &packet("type=wdata:mime=dGV4dC9wbGFpbg==", Some("YWJj")),
            false,
            false,
            Instant::now()
        )),
        "EPERM"
    );
    assert!(s.write.is_none());
}

#[test]
fn denial_and_foreign_continuations_never_overwrite_active_write() {
    let mut s = session();
    assert_eq!(
        status(s.handle(
            &packet("type=write:id=x", None),
            false,
            false,
            Instant::now()
        )),
        "EPERM"
    );
    handle(&mut s, "type=write:id=a", None);
    assert_eq!(status(handle(&mut s, "type=write:id=b", None)), "EBUSY");
    assert_eq!(
        status(handle(
            &mut s,
            "type=wdata:id=b:mime=dGV4dC9wbGFpbg==",
            Some("YWJj")
        )),
        "EBUSY"
    );
    assert_eq!(s.write.as_ref().unwrap().id, "a");
    s.generation.fetch_add(1, Ordering::AcqRel);
    assert!(handle(&mut s, "type=wdata:id=a", None).is_none());
}

#[test]
fn malformed_continuation_aborts_not_prefix_commit() {
    for fields in [
        packet("type=wdata:type=wdata", None),
        vec![b"type=wdata".to_vec(), vec![], b"overflow".to_vec()],
        packet("type=wdata:name=%%%%", None),
    ] {
        let mut s = session();
        handle(&mut s, "type=write:id=x", None);
        assert_eq!(
            status(s.handle(&fields, false, true, Instant::now())),
            "EINVAL"
        );
        assert!(handle(&mut s, "type=wdata", None).is_none());
    }
}

#[test]
fn invalid_continuation_location_aborts_and_drains_without_extra_errors() {
    let mut s = session();
    handle(&mut s, "type=write:id=owner", None);
    handle(&mut s, "type=wdata:mime=dGV4dC9wbGFpbg==", Some("YWJj"));
    assert_eq!(
        status(handle(
            &mut s,
            "type=wdata:loc=bogus:mime=dGV4dC9wbGFpbg==",
            Some("ZA==")
        )),
        "ENOSYS"
    );
    assert!(handle(&mut s, "type=wdata:loc=bogus", None).is_none());
    assert!(handle(&mut s, "type=wdata", None).is_none());
    assert!(handle(&mut s, "type=write:id=new", None).is_none());
    assert!(matches!(
        handle(&mut s, "type=wdata", None),
        Some(ClipboardAction::Write { .. })
    ));
}

#[test]
fn malformed_initial_write_is_correlated_einval_but_bad_read_does_not_abort_write() {
    let mut s = session();
    for meta in [
        "type=write:id=x!:pw=%%%%:name=eA==",
        "type=write:id=x!:name=%%%%",
        "type=write:id=x!:broken",
    ] {
        match handle(&mut s, meta, None) {
            Some(ClipboardAction::Reply(reply)) => {
                assert_eq!(reply.error, Some("EINVAL"));
                assert_eq!(reply.id, "x");
            }
            _ => panic!("fresh invalid write must fail explicitly"),
        }
        assert!(s.write.is_none());
    }
    handle(&mut s, "type=write:id=good", None);
    assert!(handle(&mut s, "type=read:type=read", Some("%%%%")).is_none());
    assert_eq!(s.write.as_ref().unwrap().id, "good");
    assert!(matches!(
        handle(&mut s, "type=wdata", None),
        Some(ClipboardAction::Write { .. })
    ));
}

#[test]
fn accepts_full_64_mib_and_rejects_next_byte_without_commit() {
    // Exercise the real streaming decoder and configured boundary, not a reduced test cap.
    let mut s = session();
    handle(&mut s, "type=write", None);
    let chunk = STANDARD.encode(vec![42; 3072]); // no padding until final quartet
    let mut count = 0;
    while count + 3072 <= WRITE_LIMIT {
        assert!(handle(
            &mut s,
            "type=wdata:mime=YXBwbGljYXRpb24vb2N0ZXQtc3RyZWFt",
            Some(&chunk)
        )
        .is_none());
        count += 3072;
    }
    let tail = STANDARD.encode(vec![42; WRITE_LIMIT - count]);
    assert!(handle(
        &mut s,
        "type=wdata:mime=YXBwbGljYXRpb24vb2N0ZXQtc3RyZWFt",
        Some(&tail)
    )
    .is_none());
    match handle(&mut s, "type=wdata", None) {
        Some(ClipboardAction::Write { data, busy, .. }) => {
            assert_eq!(data.values().next().unwrap().len(), WRITE_LIMIT);
            busy.store(false, Ordering::Release);
        }
        _ => panic!("minimum capacity"),
    }
    handle(&mut s, "type=write", None);
    let write = s.write.as_mut().unwrap();
    write.size = WRITE_LIMIT;
    assert_eq!(
        status(handle(
            &mut s,
            "type=wdata:mime=dGV4dC9wbGFpbg==",
            Some("YQ==")
        )),
        "EFBIG"
    );
    assert!(handle(&mut s, "type=wdata", None).is_none());
}

/// Simulates short writes, interruption, and revocation in the middle of a frame.
struct PartialWriter {
    bytes: Vec<u8>,
    interrupt: bool,
    revoke: Option<Arc<AtomicU64>>,
}
impl Write for PartialWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.interrupt {
            self.interrupt = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let n = bytes.len().min(7);
        self.bytes.extend_from_slice(&bytes[..n]);
        if let Some(epoch) = self.revoke.take() {
            epoch.fetch_add(1, Ordering::AcqRel);
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn responses_preserve_short_writes_and_independently_pad_chunks() {
    let s = session();
    let mut reply = s.response("read", "id".into(), None);
    reply
        .data
        .insert("image/png".into(), Arc::new(vec![5; 4097]));
    let mut writer = PartialWriter {
        bytes: vec![],
        interrupt: true,
        revoke: None,
    };
    reply.write_to(&mut writer).unwrap();
    let wire = String::from_utf8(writer.bytes).unwrap();
    assert!(wire.contains(&format!(";{}\x1b\\", STANDARD.encode(vec![5; 4096]))));
    assert!(wire.contains(";BQ==\x1b\\"));
    assert_eq!(wire.matches(":id=id").count(), 4);
    assert!(wire.ends_with(":status=DONE\x1b\\"));
}

#[test]
fn revoke_finishes_started_frame_but_never_data_or_done() {
    let s = session();
    let mut reply = s.response("read", "old".into(), None);
    reply
        .data
        .insert("image/png".into(), Arc::new(vec![7; 4097]));
    let mut writer = PartialWriter {
        bytes: vec![],
        interrupt: false,
        revoke: Some(s.generation.clone()),
    };
    reply.write_to(&mut writer).unwrap();
    assert_eq!(writer.bytes, b"\x1b]5522;type=read:id=old:status=OK\x1b\\");
}

#[test]
fn zero_write_is_failure_never_done() {
    struct Zero;
    impl Write for Zero {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Ok(0)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let s = session();
    assert_eq!(
        s.response("read", "".into(), None)
            .write_to(&mut Zero)
            .unwrap_err()
            .kind(),
        io::ErrorKind::WriteZero
    );
}

#[test]
fn shared_v2_wire_fixtures_frame_at_arbitrary_transport_boundaries_and_redact_debug() {
    use wezterm_escape_parser::parser::Parser;
    use wezterm_escape_parser::{Action, OperatingSystemCommand};
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("../../tests/data/osc5522-transcripts-v2.json")).unwrap();
    let mut count = 0;
    for case in fixtures["cases"].as_array().unwrap() {
        for step in case["steps"].as_array().unwrap() {
            let wire = match step["wire"].as_str() {
                Some(wire) if wire.starts_with("\x1b]5522;") => wire,
                _ => continue,
            };
            count += 1;
            let mut parser = Parser::new();
            let mut actions = vec![];
            for byte in wire.as_bytes() {
                actions.extend(parser.parse_as_vec(&[*byte]));
            }
            let packets: Vec<_> = actions
                .iter()
                .filter_map(|a| match a {
                    Action::OperatingSystemCommand(osc) => match &**osc {
                        OperatingSystemCommand::Clipboard5522(packet) => Some(packet),
                        _ => None,
                    },
                    _ => None,
                })
                .collect();
            assert_eq!(packets.len(), 1, "{}", case["name"]);
            let debug = format!("{actions:?}");
            assert!(!debug.contains("pw="));
            assert!(!debug.contains("mime="));
            assert!(!debug.contains("fixture-token"));
        }
    }
    assert!(count > 60);
}

fn parse_into_session(
    parser: &mut wezterm_escape_parser::parser::Parser,
    s: &mut ClipboardSession,
    bytes: &[u8],
) -> Vec<ClipboardAction> {
    use wezterm_escape_parser::{Action, OperatingSystemCommand};
    parser
        .parse_as_vec(bytes)
        .into_iter()
        .filter_map(|a| match a {
            Action::OperatingSystemCommand(osc) => match *osc {
                OperatingSystemCommand::Clipboard5522(packet) => {
                    s.handle(&packet.0, false, true, Instant::now())
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

#[test]
fn parser_never_normalizes_controls_or_commits_canceled_write_terminators() {
    use wezterm_escape_parser::parser::Parser;
    for control in [0, 9, 10, 13, 127] {
        let mut parser = Parser::new();
        let mut s = session();
        parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=write:id=x\x1b\\");
        let mut packet = b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;YW".to_vec();
        packet.push(control);
        packet.extend_from_slice(b"Jj\x1b\\");
        let actions = parse_into_session(&mut parser, &mut s, &packet);
        assert_eq!(status(actions.into_iter().next()), "EINVAL");
        assert!(parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=wdata\x1b\\").is_empty());
    }
    for terminator in [b"\x18".as_slice(), b"\x1a", b"\x1b[0m"] {
        let mut parser = Parser::new();
        let mut s = session();
        parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=write:id=x\x1b\\");
        let mut packet = b"\x1b]5522;type=wdata".to_vec();
        packet.extend_from_slice(terminator);
        let actions = parse_into_session(&mut parser, &mut s, &packet);
        assert_eq!(status(actions.into_iter().next()), "EINVAL");
        assert!(s.write.is_none());
    }
    let mut parser = Parser::new();
    let mut s = session();
    parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=write:id=x\x1b\\");
    assert!(parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=wdata\x1b").is_empty());
    assert!(s.write.is_some());
    assert!(matches!(
        parse_into_session(&mut parser, &mut s, b"\\").pop(),
        Some(ClipboardAction::Write { .. })
    ));
    // Reuse the same parser across legacy OSC and canceled/split clipboard
    // frames: deferred ST must not retain stale offsets or alter legacy OSC.
    for packet in [
        b"\x1b]0;legacy\x1b\\".as_slice(),
        b"\x1b]5522;type=wdata\x18",
        b"\x1b]0;next\x07",
        b"\x1b]5522;type=read:id=f;Lg==\x1b\\",
    ] {
        for byte in packet {
            parse_into_session(&mut parser, &mut s, &[*byte]);
        }
    }
}

#[test]
fn oversized_frame_aborts_write_and_does_not_dispatch_truncated_base64() {
    let mut parser = wezterm_escape_parser::parser::Parser::new();
    let mut s = session();
    parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=write:id=x\x07");
    let mut bytes = b"\x1b]5522;type=wdata:mime=dGV4dC9wbGFpbg==;".to_vec();
    bytes.extend(std::iter::repeat_n(b'A', 160 * 1024));
    bytes.extend_from_slice(b"\x1b\\");
    assert_eq!(
        status(
            parse_into_session(&mut parser, &mut s, &bytes)
                .into_iter()
                .next()
        ),
        "EINVAL"
    );
    assert!(s.write.is_none());
}

#[test]
fn file_packets_keep_parser_corruption_and_fragmentation_fences() {
    let mime = STANDARD.encode("application/vnd.tau.file-export-v1");
    for suffix in [
        b"\0\x1b\\".as_slice(),
        b"\n\x1b\\",
        b"\x18",
        b"\x1a",
        b"\x1b[0m",
    ] {
        let mut parser = wezterm_escape_parser::parser::Parser::new();
        let mut s = session();
        parse_into_session(&mut parser, &mut s, b"\x1b]5522;type=write-file:id=f\x1b\\");
        let mut bytes = format!("\x1b]5522;type=write-file-data:id=f:mime={mime};YW").into_bytes();
        bytes.extend_from_slice(suffix);
        assert_eq!(
            status(parse_into_session(&mut parser, &mut s, &bytes).pop()),
            "EINVAL"
        );
        assert!(s.write.is_none());
    }
    let mut parser = wezterm_escape_parser::parser::Parser::new();
    let mut s = session();
    let wire = format!(
        "\x1b]5522;type=write-file:id=f\x1b\\\
         \x1b]5522;type=write-file-data:id=f:mime={mime};YWJj\x1b\\\
         \x1b]5522;type=write-file-data:id=f\x1b\\"
    );
    let mut actions = vec![];
    for byte in wire.bytes() {
        actions.extend(parse_into_session(&mut parser, &mut s, &[byte]));
    }
    assert!(matches!(
        actions.pop(),
        Some(ClipboardAction::Write {
            file_export: true,
            ..
        })
    ));
    assert!(actions.is_empty());
}
