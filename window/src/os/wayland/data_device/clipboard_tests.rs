use super::super::copy_and_paste::CopyPasteOffer;
use super::*;
use std::sync::atomic::Ordering;

#[test]
fn toolkit_null_event_is_forwarded_after_native_cache_clear() {
    struct State {
        cached: bool,
        offer: CopyPasteOffer,
    }
    let mut state = State {
        cached: true,
        offer: CopyPasteOffer::default(),
    };
    let generation = state.offer.generation.clone();
    let epoch = generation.load(Ordering::Acquire);
    dispatch_with_clear_notification(
        &mut state,
        wl_data_device::Event::Selection { id: None },
        |state, _| {
            // SCTK0.19.2 clears the object but omits the user's callback.
            state.cached = false;
        },
        |state| {
            assert!(!state.cached, "do not inspect the toolkit's old cache");
            state.offer.confirm_selection(None);
        },
    );
    assert_ne!(generation.load(Ordering::Acquire), epoch);
    dispatch_with_clear_notification(
        &mut state,
        wl_data_device::Event::Leave,
        |_, _| {},
        |_| panic!("unrelated event must not synthesize selection notification"),
    );
}
