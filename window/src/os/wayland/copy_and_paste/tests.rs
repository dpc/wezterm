use super::*;
use crate::clipboard::ClipboardError;

#[test]
fn clear_callback_revokes_retained_generation_even_without_replacement() {
    let mut offers = CopyPasteOffer::default();
    let generation = offers.generation.clone();
    let epoch = generation.load(Ordering::Acquire);
    offers.confirm_selection(None);
    assert!(offers.data_offer.is_none());
    assert_ne!(generation.load(Ordering::Acquire), epoch);
    assert!(!offers.pending_clipboard);
}

#[test]
fn local_publish_cannot_relabel_cached_owner_before_confirmation() {
    let mut offers = CopyPasteOffer::default();
    for clipboard in [Clipboard::Clipboard, Clipboard::PrimarySelection] {
        offers.publication_started(clipboard);
        // This returns before looking up any live connection/device cache.
        let error = match offers.retain_offer(clipboard) {
            Ok(_) => panic!("old native offer must not be rebound"),
            Err(error) => error,
        };
        assert!(matches!(
            error.downcast_ref::<ClipboardError>(),
            Some(ClipboardError::Temporary)
        ));
        assert!(offers.get_clipboard_data(clipboard).is_err());
    }
    offers.confirm_selection(None);
    assert!(!offers.pending_clipboard);
    assert!(offers.pending_primary);
}
