//! Real WindowOps callbacks with the same state/queue borrow boundary as dispatch.
//! A tiny wire peer supplies native offers; no compositor, desktop, or GL is used.
use super::*;
use crate::clipboard::{ClipboardError, ClipboardOffer};
use smithay_client_toolkit::seat::{Capability, SeatHandler};
use std::io::Write;
use std::os::unix::net::UnixStream;

/// Requires the API to complete at the callback boundary, without queued work.
fn ready<T: Send + 'static>(future: Future<T>) -> anyhow::Result<T> {
    promise::spawn::block_on(futures_lite::future::poll_once(future))
        .expect("native offer retention and publication must complete synchronously")
}

/// Writes one native-endian Wayland wire event.
fn event(socket: &mut UnixStream, object: u32, opcode: u16, payload: &[u8]) {
    socket.write_all(&object.to_ne_bytes()).unwrap();
    socket
        .write_all(&(((payload.len() as u32 + 8) << 16) | u32::from(opcode)).to_ne_bytes())
        .unwrap();
    socket.write_all(payload).unwrap();
}

/// Encodes a Wayland string, including its NUL and alignment padding.
fn string(value: &str) -> Vec<u8> {
    let mut bytes = ((value.len() + 1) as u32).to_ne_bytes().to_vec();
    bytes.extend_from_slice(value.as_bytes());
    bytes.push(0);
    bytes.resize((bytes.len() + 3) & !3, 0);
    bytes
}

/// Owns a private wire transport and the thread-local connection used by WindowOps.
struct Fixture {
    /// Actual client-side state, not a replacement RefCell model.
    conn: Rc<WaylandConnection>,
    /// Server endpoint used to inject selection events.
    peer: UnixStream,
}

impl Fixture {
    fn new() -> Self {
        let (client, mut peer) = UnixStream::pair().unwrap();
        let mut reader = peer.try_clone().unwrap();
        let mut writer = peer.try_clone().unwrap();
        // Only registry discovery and sync replies are needed. Other requests
        // (including shm setup) are accepted without creating desktop resources.
        std::thread::spawn(move || {
            let mut header = [0; 8];
            while reader.read_exact(&mut header).is_ok() {
                let object = u32::from_ne_bytes(header[..4].try_into().unwrap());
                let word = u32::from_ne_bytes(header[4..].try_into().unwrap());
                let opcode = word & 0xffff;
                let mut payload = vec![0; (word >> 16) as usize - 8];
                reader.read_exact(&mut payload).unwrap();
                if object != 1 {
                    continue;
                }
                let id = u32::from_ne_bytes(payload[..4].try_into().unwrap());
                match opcode {
                    0 => {
                        event(&mut writer, id, 0, &0u32.to_ne_bytes());
                        event(&mut writer, 1, 1, &id.to_ne_bytes());
                    }
                    1 => {
                        for (index, (name, version)) in [
                            ("wl_compositor", 4u32),
                            ("wl_subcompositor", 1),
                            ("wl_shm", 1),
                            ("xdg_wm_base", 1),
                            ("wl_data_device_manager", 3),
                            ("zwp_primary_selection_device_manager_v1", 1),
                            ("wl_seat", 5),
                        ]
                        .iter()
                        .enumerate()
                        {
                            let mut payload = (index as u32 + 1).to_ne_bytes().to_vec();
                            payload.extend(string(name));
                            payload.extend(version.to_ne_bytes());
                            event(&mut writer, id, 0, &payload);
                        }
                    }
                    _ => panic!("unexpected display request"),
                }
            }
        });
        let conn = Rc::new(
            WaylandConnection::from_connection(WConnection::from_socket(client).unwrap()).unwrap(),
        );
        {
            let qh = conn.event_queue.borrow().handle();
            let mut state = conn.wayland_state.borrow_mut();
            let seat = state.seat.seats().next().unwrap();
            // The production capability callback creates both selection devices.
            state.new_capability(&conn.connection, &qh, seat, Capability::Keyboard);
        }
        crate::connection::set_test_connection(Rc::new(Connection::Wayland(conn.clone())));
        // Initial offers have real toolkit protocol objects and MIME lists.
        Self::selection(&conn, &mut peer, false, 0xff00_0000, "text/plain");
        Self::selection(&conn, &mut peer, true, 0xff00_0001, "image/png");
        let fixture = Self { conn, peer };
        fixture.roundtrip();
        fixture
    }

    /// Sends an actual data-offer/selection sequence (or a clear).
    fn selection(
        conn: &WaylandConnection,
        peer: &mut UnixStream,
        primary: bool,
        offer: u32,
        mime: &str,
    ) {
        let state = conn.wayland_state.borrow();
        let device = if primary {
            state
                .primary_selection_device
                .as_ref()
                .unwrap()
                .inner()
                .id()
        } else {
            state.data_device.as_ref().unwrap().inner().id()
        };
        if offer != 0 {
            event(peer, device.protocol_id(), 0, &offer.to_ne_bytes());
            event(peer, offer, 0, &string(mime));
        }
        event(
            peer,
            device.protocol_id(),
            if primary { 1 } else { 5 },
            &offer.to_ne_bytes(),
        );
    }

    fn roundtrip(&self) {
        self.conn
            .event_queue
            .borrow_mut()
            .roundtrip(&mut *self.conn.wayland_state.borrow_mut())
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        crate::connection::shutdown();
    }
}

#[test]
fn clipboard_window_callback_retains_native_offers_during_dispatch() {
    let mut fixture = Fixture::new();
    let retained = Rc::new(RefCell::new(Vec::<Arc<dyn ClipboardOffer>>::new()));
    let callback_retained = retained.clone();
    let mut events = WindowEventSender::new(move |event, window| {
        if !matches!(event, WindowEvent::KeyEvent(_)) {
            return;
        }
        for (clipboard, expected) in [
            (Clipboard::Clipboard, "text/plain"),
            (Clipboard::PrimarySelection, "image/png"),
        ] {
            // This is the same synchronous WindowOps call made by the GUI's
            // paste key handler. It must complete before dispatch resumes.
            let offer = ready(window.clipboard_offer(clipboard)).unwrap();
            assert_eq!(offer.mime_types(), &[expected]);
            assert!(offer.valid());
            callback_retained.borrow_mut().push(offer);
        }
        // Direct reentrant publication is explicitly unavailable, not a panic,
        // and must neither enqueue a late write nor revoke the existing offer.
        let error =
            ready(window.set_mime_clipboard(Clipboard::Clipboard, Default::default())).unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ClipboardError>(),
            Some(ClipboardError::Temporary)
        ));
    });
    events.assign_window(Window::Wayland(WaylandWindow(0)));
    let mapper =
        KeyboardWithFallback::new(crate::x11::keyboard::Keyboard::new_default().unwrap()).unwrap();
    {
        let _queue = fixture.conn.event_queue.borrow_mut();
        let _state = fixture.conn.wayland_state.borrow_mut();
        // Linux KEY_V goes through the production keyboard translation and
        // WindowEventSender callback, under the actual dispatch-owned cells.
        mapper.process_wayland_key(47, true, &mut events);
    }
    assert_eq!(retained.borrow().len(), 2);
    assert!(retained.borrow().iter().all(|offer| offer.valid()));
    // The queue and state can be borrowed independently: check the state-only
    // path too, and ensure a rejected write changes neither native identity.
    let window = WaylandWindow(0);
    {
        let _state = fixture.conn.wayland_state.borrow_mut();
        let error =
            ready(window.set_mime_clipboard(Clipboard::PrimarySelection, Default::default()))
                .unwrap_err();
        assert!(matches!(
            error.downcast_ref::<ClipboardError>(),
            Some(ClipboardError::Temporary)
        ));
    }
    assert!(retained.borrow().iter().all(|offer| offer.valid()));
    for (primary, id) in [(false, 0xff00_0002), (true, 0xff00_0003)] {
        Fixture::selection(&fixture.conn, &mut fixture.peer, primary, id, "new/owner");
    }
    fixture.roundtrip();
    for offer in retained.borrow().iter() {
        assert!(
            !offer.valid(),
            "replacement must revoke the captured identity"
        );
    }
    assert_eq!(retained.borrow()[0].mime_types(), &["text/plain"]);
    assert_eq!(retained.borrow()[1].mime_types(), &["image/png"]);
    let replacements = [Clipboard::Clipboard, Clipboard::PrimarySelection]
        .map(|clipboard| ready(window.clipboard_offer(clipboard)).unwrap());
    for offer in &replacements {
        assert_eq!(offer.mime_types(), &["new/owner"]);
        assert!(offer.valid());
    }
    for primary in [false, true] {
        Fixture::selection(&fixture.conn, &mut fixture.peer, primary, 0, "");
    }
    fixture.roundtrip();
    assert!(replacements.iter().all(|offer| !offer.valid()));
    // Outside dispatch, native publication remains synchronous. Generation
    // revocation is observable even before polling its returned future.
    for clipboard in [Clipboard::Clipboard, Clipboard::PrimarySelection] {
        let before = ready(window.clipboard_offer(clipboard)).unwrap();
        assert!(before.mime_types().is_empty());
        let published = window.set_mime_clipboard(clipboard, Default::default());
        assert!(!before.valid());
        ready(published).unwrap();
    }
}
