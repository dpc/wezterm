use super::*;
use window::clipboard::ClipboardData;

struct Offer {
    /// Original file-body inventory, independent of clipboard owner lookups.
    types: Vec<String>,
}

impl ClipboardOffer for Offer {
    fn mime_types(&self) -> &[String] {
        &self.types
    }
    fn valid(&self) -> bool {
        true
    }
    fn read(
        &self,
        _: Vec<String>,
        _: usize,
        _: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> promise::Future<ClipboardData> {
        promise::Future::ok(ClipboardData::new())
    }
}

#[test]
fn unavailable_read_preserves_snapshot_then_available_read_consumes_it() {
    let mime = "application/vnd.tau.files-v1";
    let offer: Arc<dyn ClipboardOffer> = Arc::new(Offer {
        types: vec![mime.to_owned()],
    });
    let weak = Arc::downgrade(&offer);
    let mut session = Session {
        protocol: ClipboardSession::new(Arc::new(AtomicU64::new(0))),
        offer: Some(offer),
        epoch: 0,
    };
    session
        .protocol
        .offer("c2VjcmV0".into(), vec![mime.into()], false, Instant::now())
        .unwrap();
    for (requested, remains) in [("application/unavailable", true), (mime, false)] {
        let fields = vec![
            b"type=read:pw=c2VjcmV0:name=eA==".to_vec(),
            STANDARD.encode(requested).into_bytes(),
        ];
        match session
            .protocol
            .handle_for_epoch(&fields, false, false, Instant::now(), 0)
            .unwrap()
        {
            ClipboardAction::Read {
                retained,
                types,
                busy,
                ..
            } => {
                assert!(retained);
                assert_eq!(types.is_empty(), remains);
                busy.store(false, Ordering::Release);
            }
            _ => panic!("expected authorized read"),
        }
        let read_offer = session.retained_read_offer().unwrap();
        assert!(Arc::ptr_eq(&read_offer, &weak.upgrade().unwrap()));
        assert_eq!(session.offer.is_some(), remains);
        assert_eq!(session.protocol.has_grant(), remains);
        drop(read_offer);
    }
    assert!(weak.upgrade().is_none());
}
