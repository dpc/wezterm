use anyhow::{anyhow, bail};
use smithay_client_toolkit as toolkit;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use toolkit::data_device_manager::data_offer::SelectionOffer;
use toolkit::data_device_manager::{ReadPipe, WritePipe};
use toolkit::primary_selection::device::PrimarySelectionDeviceHandler;
use toolkit::primary_selection::selection::PrimarySelectionSourceHandler;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use wayland_protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1;

use crate::{Clipboard, ConnectionOps};

use super::data_device::TEXT_MIME_TYPE;
use super::state::WaylandState;
#[cfg(test)]
mod tests;

/// Holds the most recent clipboard offer received from another Wayland client.
/// Updated on every `wl_data_device::selection` event.
#[derive(Default)]
pub struct CopyPasteOffer {
    data_offer: Option<SelectionOffer>,
    /// Revoke retained clipboard identities whenever the compositor replaces/clears one.
    pub(super) generation: Arc<AtomicU64>,
    /// Independent primary selection identity.
    pub(super) primary_generation: Arc<AtomicU64>,
    /// Never pair the previous native object with a locally advanced epoch.
    pending_clipboard: bool,
    /// The primary device retains its old cache until the compositor confirms.
    pending_primary: bool,
    /// Epoch assigned to the cached ordinary offer, never inferred later.
    clipboard_epoch: u64,
    /// Epoch assigned at the most recent primary selection notification.
    primary_epoch: u64,
}

impl std::fmt::Debug for CopyPasteOffer {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> Result<(), std::fmt::Error> {
        fmt.debug_struct("CopyPasteOffer")
            .field("data_offer", &self.data_offer.is_some())
            .finish()
    }
}

impl CopyPasteOffer {
    /// Creates a new empty offer.
    pub(super) fn create() -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Default::default()))
    }

    /// Returns a pipe to read the current clipboard contents for the given clipboard type.
    pub(super) fn get_clipboard_data(&self, clipboard: Clipboard) -> anyhow::Result<ReadPipe> {
        self.check_confirmed(clipboard)?;
        let conn = crate::Connection::get().unwrap().wayland();
        let wayland_state = conn.wayland_state.borrow();
        let primary_selection = if let Clipboard::PrimarySelection = clipboard {
            Some(
                wayland_state
                    .primary_selection_device
                    .as_ref()
                    .ok_or(crate::clipboard::ClipboardError::Unsupported)?,
            )
        } else {
            None
        };

        match primary_selection {
            Some(primary_selection) => {
                let offer = primary_selection
                    .data()
                    .selection_offer()
                    .ok_or_else(|| anyhow!("no primary selection offer"))?;
                if !offer.with_mime_types(|m| m.iter().any(|m| m == TEXT_MIME_TYPE)) {
                    bail!("selection has no UTF-8 text");
                }
                let pipe = offer.receive(TEXT_MIME_TYPE.to_string())?;
                Ok(pipe)
            }
            None => {
                let offer = self
                    .data_offer
                    .as_ref()
                    .ok_or_else(|| anyhow!("no data offer"))?;
                if !offer.with_mime_types(|m| m.iter().any(|m| m == TEXT_MIME_TYPE)) {
                    bail!("selection has no UTF-8 text");
                }
                let pipe = offer.receive(TEXT_MIME_TYPE.to_string())?;
                Ok(pipe)
            }
        }
    }

    /// Claims clipboard ownership and advertises `data` as the current selection.
    pub(super) fn set_clipboard_data(&mut self, clipboard: Clipboard, data: String) {
        let data =
            std::iter::once((TEXT_MIME_TYPE.to_owned(), Arc::new(data.into_bytes()))).collect();
        if let Err(err) = self.set_mime_data(clipboard, data) {
            log::warn!("could not set clipboard: {err}");
        }
    }

    /// Atomically publishes all formats, preserving alias allocations.
    pub(super) fn set_mime_data(
        &mut self,
        clipboard: Clipboard,
        data: crate::clipboard::ClipboardData,
    ) -> anyhow::Result<()> {
        let conn = crate::Connection::get().unwrap().wayland();
        let qh = conn.event_queue.borrow().handle();
        let mut wayland_state = conn.wayland_state.borrow_mut();
        let last_serial = *wayland_state.last_serial.borrow();

        let primary_selection = if let Clipboard::PrimarySelection = clipboard {
            Some(
                wayland_state
                    .primary_selection_device
                    .as_ref()
                    .ok_or(crate::clipboard::ClipboardError::Unsupported)?,
            )
        } else {
            None
        };

        match primary_selection {
            Some(primary_selection) => {
                let manager = wayland_state.primary_selection_manager.as_ref().unwrap();
                let source = manager.create_selection_source(&qh, data.keys().map(String::as_str));
                source.set_selection(&primary_selection, last_serial);
                wayland_state
                    .primary_selection_source
                    .replace((source, data));
                self.publication_started(clipboard);
            }
            None => {
                let data_device = &wayland_state.data_device;
                let source = wayland_state
                    .data_device_manager_state
                    .create_copy_paste_source(&qh, data.keys().map(String::as_str));
                source.set_selection(
                    data_device
                        .as_ref()
                        .ok_or(crate::clipboard::ClipboardError::Temporary)?,
                    last_serial,
                );
                wayland_state.copy_paste_source.replace((source, data));
                self.publication_started(clipboard);
            }
        }
        Ok(())
    }

    /// Stores the latest selection offer from the compositor, replacing any previous one.
    pub(super) fn confirm_selection(&mut self, offer: Option<SelectionOffer>) {
        self.clipboard_epoch = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.pending_clipboard = false;
        self.data_offer = offer;
    }

    fn publication_started(&mut self, clipboard: Clipboard) {
        match clipboard {
            Clipboard::Clipboard => {
                self.generation.fetch_add(1, Ordering::AcqRel);
                self.data_offer = None;
                self.pending_clipboard = true;
            }
            Clipboard::PrimarySelection => {
                self.primary_generation.fetch_add(1, Ordering::AcqRel);
                self.pending_primary = true;
            }
        }
    }

    fn check_confirmed(&self, clipboard: Clipboard) -> anyhow::Result<()> {
        if match clipboard {
            Clipboard::Clipboard => {
                self.pending_clipboard
                    || self.clipboard_epoch != self.generation.load(Ordering::Acquire)
            }
            Clipboard::PrimarySelection => {
                self.pending_primary
                    || self.primary_epoch != self.primary_generation.load(Ordering::Acquire)
            }
        } {
            return Err(crate::clipboard::ClipboardError::Temporary.into());
        }
        Ok(())
    }

    /// Captures native identity and all format names without reading content.
    pub(super) fn retain_offer(
        &self,
        clipboard: Clipboard,
    ) -> anyhow::Result<Arc<dyn crate::clipboard::ClipboardOffer>> {
        self.check_confirmed(clipboard)?;
        use super::mime_offer::{MimeOffer, NativeOffer};
        let (native, generation, epoch) = match clipboard {
            Clipboard::Clipboard => (
                self.data_offer.clone().map(NativeOffer::Clipboard),
                self.generation.clone(),
                self.clipboard_epoch,
            ),
            Clipboard::PrimarySelection => {
                let conn = crate::Connection::get().unwrap().wayland();
                let state = conn.wayland_state.borrow();
                let device = state
                    .primary_selection_device
                    .as_ref()
                    .ok_or(crate::clipboard::ClipboardError::Unsupported)?;
                (
                    device.data().selection_offer().map(NativeOffer::Primary),
                    self.primary_generation.clone(),
                    self.primary_epoch,
                )
            }
        };
        Ok(Arc::new(MimeOffer::new(native, generation, epoch)))
    }
}

pub(super) fn write_selection_to_pipe(fd: WritePipe, data: Arc<Vec<u8>>) {
    super::selection_sender::send(fd, data);
}

impl PrimarySelectionDeviceHandler for WaylandState {
    fn selection(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        _primary_selection_device: &ZwpPrimarySelectionDeviceV1,
    ) {
        let mut offers = self.copy_paste_offer.lock().unwrap();
        offers.primary_epoch = offers.primary_generation.fetch_add(1, Ordering::AcqRel) + 1;
        offers.pending_primary = false;
    }
}

impl PrimarySelectionSourceHandler for WaylandState {
    fn send_request(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
        mime: String,
        write_pipe: toolkit::data_device_manager::WritePipe,
    ) {
        if let Some((ps_source, data)) = &self.primary_selection_source {
            if ps_source.inner() != source {
                return;
            }
            if let Some(bytes) = data.get(&mime) {
                write_selection_to_pipe(write_pipe, bytes.clone());
            }
        }
    }

    fn cancelled(
        &mut self,
        _conn: &wayland_client::Connection,
        _qh: &wayland_client::QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        if self
            .primary_selection_source
            .as_ref()
            .is_some_and(|(current, _)| current.inner() == source)
        {
            self.primary_selection_source.take();
        }
        source.destroy();
    }
}
