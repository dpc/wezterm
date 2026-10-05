//! OSC5522 GUI attachment. Native offers stay inside this process.
use super::{TermWindow, TermWindowNotif};
use base64::{engine::general_purpose::STANDARD, Engine};
use mux::pane::{Pane, PaneId};
use mux::Mux;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use wezterm_term::clipboard::{
    ClipboardAction, ClipboardHandler, ClipboardResponse, ClipboardSession,
};
use window::clipboard::{location, ClipboardError, ClipboardOffer};
use window::{Clipboard, Window, WindowOps};

/// Native offer and protocol state for one pane/window attachment.
struct Session {
    /// Protocol-only state and lifetime checks.
    protocol: ClipboardSession,
    /// Exact gesture-time native offer; never replaced by an ambient read.
    offer: Option<Arc<dyn ClipboardOffer>>,
    /// Epoch of the retained offer.
    epoch: u64,
}

impl Session {
    fn retained_read_offer(&mut self) -> Option<Arc<dyn ClipboardOffer>> {
        if self.protocol.has_grant() {
            // An authorized read of only unavailable types is empty success
            // and does not consume the grant or its native identity.
            self.offer.clone()
        } else {
            self.offer.take()
        }
    }
}

/// A queued GUI request cannot retain unbounded parser input under UI backpressure.
struct RequestTicket(Arc<AtomicUsize>);
impl Drop for RequestTicket {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Thread-safe bridge from terminal parsing to its owning GUI window.
pub(super) struct MimeClipboard {
    /// Notification destination, not a global clipboard endpoint.
    window: Window,
    /// Original pane identity.
    pane_id: PaneId,
    /// Synchronously observable DEC mode.
    enabled: AtomicBool,
    /// Permanent attachment teardown; retained panes must stop advertising support.
    attached: AtomicBool,
    /// Immediate revocation across parser and GUI threads.
    generation: Arc<AtomicU64>,
    /// Linearizes native publication against parser-thread revocation.
    commit: Mutex<()>,
    /// Mutable state, accessed on GUI tasks or under the mutex.
    session: Mutex<Session>,
    /// Avoid overlapping gesture admission before the native offer resolves.
    acquiring: AtomicBool,
    /// At most one weak expiry task per attachment, never one per gesture.
    expiry_task: AtomicBool,
    /// In-flight parser-to-GUI requests, including currently executing work.
    queued: Arc<AtomicUsize>,
}

impl ClipboardHandler for MimeClipboard {
    fn supported(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }
    fn detach(&self) {
        let _commit = self.commit.lock().unwrap();
        self.attached.store(false, Ordering::Release);
        self.enabled.store(false, Ordering::Release);
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.session.lock().unwrap().offer = None;
    }
    fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    fn set_enabled(&self, enabled: bool) {
        let _commit = self.commit.lock().unwrap();
        self.enabled
            .store(enabled && self.supported(), Ordering::Release);
        if !enabled {
            self.generation.fetch_add(1, Ordering::AcqRel);
            self.session.lock().unwrap().offer = None;
        }
    }
    fn invalidate(&self) {
        let _commit = self.commit.lock().unwrap();
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.session.lock().unwrap().offer = None;
    }
    fn handle(&self, fields: Vec<Vec<u8>>) {
        if !self.supported() {
            return;
        }
        if fields.iter().map(Vec::len).sum::<usize>() > 128 * 1024
            || self
                .queued
                .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                    (n < 64).then_some(n + 1)
                })
                .is_err()
        {
            // Dropping a write chunk must never let a truncated assembly commit.
            // Fail this generation closed; already queued old work is suppressed.
            self.invalidate();
            return;
        }
        let ticket = RequestTicket(self.queued.clone());
        let pane_id = self.pane_id;
        let epoch = self.generation.load(Ordering::Acquire);
        let generation = self.generation.clone();
        self.window
            .notify(TermWindowNotif::Apply(Box::new(move |tw| {
                let _ticket = ticket;
                if let Some(handler) = tw.mime_clipboards.get(&pane_id).cloned() {
                    if Arc::ptr_eq(&handler.generation, &generation)
                        && generation.load(Ordering::Acquire) == epoch
                    {
                        handler.request(tw, fields, epoch);
                    }
                }
            })));
    }
}

impl MimeClipboard {
    fn new(window: Window, pane_id: PaneId) -> Self {
        let generation = Arc::new(AtomicU64::new(0));
        Self {
            window,
            pane_id,
            enabled: AtomicBool::new(false),
            attached: AtomicBool::new(true),
            session: Mutex::new(Session {
                protocol: ClipboardSession::new(generation.clone()),
                offer: None,
                epoch: 0,
            }),
            generation,
            commit: Mutex::new(()),
            acquiring: AtomicBool::new(false),
            expiry_task: AtomicBool::new(false),
            queued: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn send(&self, response: ClipboardResponse) {
        if let Some(pane) = Mux::get().get_pane(self.pane_id) {
            pane.send_clipboard_response(response);
        }
    }

    fn active(&self, tw: &TermWindow, epoch: u64) -> bool {
        self.supported()
            && self.generation.load(Ordering::Acquire) == epoch
            && tw.focused.is_some()
            && tw
                .get_active_pane_or_overlay()
                .is_some_and(|p| p.pane_id() == self.pane_id)
    }

    fn reap(&self) {
        let mut session = self.session.lock().unwrap();
        session.protocol.expire(Instant::now());
        let epoch = self.generation.load(Ordering::Acquire);
        if session.epoch != epoch || session.offer.as_ref().is_some_and(|o| !o.valid()) {
            session.protocol.revoke_grant();
        }
        if !session.protocol.has_grant() {
            session.offer = None;
            session.epoch = epoch;
        }
    }

    fn request(self: &Arc<Self>, tw: &TermWindow, fields: Vec<Vec<u8>>, epoch: u64) {
        let action = {
            let _commit = self.commit.lock().unwrap();
            if !self.supported() || self.generation.load(Ordering::Acquire) != epoch {
                return;
            }
            self.reap();
            let mut session = self.session.lock().unwrap();
            session.protocol.handle_for_epoch(
                &fields,
                tw.config.allow_osc5522_clipboard_read && self.active(tw, epoch),
                tw.config.allow_osc5522_clipboard_write && self.active(tw, epoch),
                Instant::now(),
                epoch,
            )
        };
        let action = match action {
            Some(action) => action,
            None => return,
        };
        let this = self.clone();
        let limit = tw.config.osc5522_clipboard_read_limit;
        match action {
            ClipboardAction::Reply(reply) => self.send(reply),
            ClipboardAction::List { id, primary, busy } => {
                let future = self.window.clipboard_offer(location(primary));
                promise::spawn::spawn(async move {
                    let result = future.await;
                    let mut reply = this
                        .session
                        .lock()
                        .unwrap()
                        .protocol
                        .response("read", id, None);
                    reply.epoch = epoch;
                    reply.busy = Some(busy);
                    match result {
                        Ok(offer) => {
                            let list = offer.mime_types().join(" ");
                            if list.len() > wezterm_term::clipboard::METADATA_LIMIT {
                                reply.error = Some("EPERM");
                            } else {
                                reply
                                    .data
                                    .insert(".".to_owned(), Arc::new(list.into_bytes()));
                            }
                        }
                        Err(err) => {
                            if let Some(status) = read_error(&err) {
                                reply.error = Some(status);
                            } else {
                                log::warn!("native clipboard listing failed");
                                return;
                            }
                        }
                    }
                    this.send(reply);
                })
                .detach();
            }
            ClipboardAction::Read {
                id,
                primary,
                types,
                retained,
                busy,
            } => {
                let offer_future = if retained {
                    // Admission consumes the one-use grant. Move its snapshot
                    // into this read rather than retaining it in the session.
                    match self.session.lock().unwrap().retained_read_offer() {
                        Some(offer) if offer.valid() && self.active(tw, epoch) => {
                            promise::Future::ok(offer)
                        }
                        _ => promise::Future::err(ClipboardError::Stale.into()),
                    }
                } else {
                    self.window.clipboard_offer(location(primary))
                };
                promise::spawn::spawn(async move {
                    let mut reply = this
                        .session
                        .lock()
                        .unwrap()
                        .protocol
                        .response("read", id, None);
                    reply.epoch = epoch;
                    reply.busy = Some(busy);
                    let result = async {
                        let offer = offer_future.await?;
                        let generation = this.generation.clone();
                        let data = offer
                            .read(
                                types,
                                limit,
                                Arc::new(move || generation.load(Ordering::Acquire) == epoch),
                            )
                            .await?;
                        if !offer.valid() {
                            return Err(ClipboardError::Stale.into());
                        }
                        Ok::<_, anyhow::Error>((offer, data))
                    }
                    .await;
                    match result {
                        Ok((offer, data)) => {
                            reply.data = data;
                            reply.valid = Some(offer_validity(offer));
                        }
                        Err(err) => {
                            if let Some(status) = read_error(&err) {
                                reply.error = Some(status);
                            } else {
                                log::warn!("native clipboard read failed before response");
                                return;
                            }
                        }
                    }
                    this.send(reply);
                })
                .detach();
            }
            ClipboardAction::Write {
                id,
                primary,
                data,
                busy,
            } => {
                let mut reply = self
                    .session
                    .lock()
                    .unwrap()
                    .protocol
                    .response("write", id, None);
                reply.epoch = epoch;
                reply.busy = Some(busy);
                let future = {
                    // Native Wayland publication occurs synchronously in this
                    // method, before the returned result future is awaited.
                    // Revoke and commit share this lock; no terminal/session lock
                    // is acquired while it is held.
                    let _commit = self.commit.lock().unwrap();
                    if self.active(tw, epoch) {
                        Some(self.window.set_mime_clipboard(location(primary), data))
                    } else {
                        None
                    }
                };
                let future = match future {
                    Some(future) => future,
                    None => {
                        reply.error = Some("EPERM");
                        self.send(reply);
                        return;
                    }
                };
                promise::spawn::spawn(async move {
                    if let Err(err) = future.await {
                        reply.error = Some(
                            if matches!(
                                err.downcast_ref::<ClipboardError>(),
                                Some(ClipboardError::Unsupported)
                            ) {
                                "ENOSYS"
                            } else {
                                "EIO"
                            },
                        );
                    }
                    this.send(reply);
                })
                .detach();
            }
        }
    }

    fn paste(self: &Arc<Self>, tw: &TermWindow, clipboard: Clipboard) {
        self.reap();
        let epoch = self.generation.load(Ordering::Acquire);
        if !self.active(tw, epoch) {
            return;
        }
        if self.session.lock().unwrap().protocol.is_busy()
            || self.acquiring.swap(true, Ordering::AcqRel)
        {
            log::warn!("clipboard paste rejected: a clipboard operation is busy");
            return;
        }
        let pane = match Mux::get().get_pane(self.pane_id) {
            Some(pane) => pane,
            None => {
                self.acquiring.store(false, Ordering::Release);
                return;
            }
        };
        let permit = match pane.reserve_clipboard_response() {
            Some(permit) => permit,
            None => {
                self.acquiring.store(false, Ordering::Release);
                log::warn!("clipboard paste rejected: response transport is busy");
                return;
            }
        };
        let future = self.window.clipboard_offer(clipboard);
        #[cfg(target_os = "linux")]
        let file_paste = tw.config.enable_osc5522_file_paste;
        #[cfg(target_os = "linux")]
        let file_limit = tw.config.osc5522_clipboard_read_limit;
        let this = self.clone();
        promise::spawn::spawn(async move {
            let result = async {
                let offer = future.await?;
                #[cfg(target_os = "linux")]
                if file_paste {
                    let generation = this.generation.clone();
                    return window::clipboard_files::snapshot_offer(
                        offer,
                        file_limit,
                        Arc::new(move || generation.load(Ordering::Acquire) == epoch),
                    )
                    .await;
                }
                Ok::<_, anyhow::Error>(offer)
            }
            .await;
            this.acquiring.store(false, Ordering::Release);
            if this.generation.load(Ordering::Acquire) != epoch || !this.enabled() {
                return;
            }
            match result {
                Ok(offer) if offer.valid() => {
                    // Protocol passwords decode to UTF-8, not arbitrary random bytes.
                    let password = STANDARD.encode(format!(
                        "{:032x}{:032x}",
                        rand::random::<u128>(),
                        rand::random::<u128>()
                    ));
                    let reply = {
                        let _commit = this.commit.lock().unwrap();
                        if !this.supported()
                            || !this.enabled()
                            || this.generation.load(Ordering::Acquire) != epoch
                        {
                            return;
                        }
                        let mut session = this.session.lock().unwrap();
                        session
                            .protocol
                            .offer_for_epoch(
                                password,
                                offer.mime_types().to_vec(),
                                clipboard == Clipboard::PrimarySelection,
                                Instant::now(),
                                epoch,
                            )
                            .map(|mut reply| {
                                session.offer = Some(offer.clone());
                                session.epoch = epoch;
                                reply.epoch = epoch;
                                reply.valid = Some(offer_validity(offer));
                                reply
                            })
                    };
                    // Never acquire the terminal writer while holding commit or
                    // session locks: parser reset holds terminal then commit.
                    if let Some(reply) = reply {
                        pane.send_reserved_clipboard_response(reply, permit);
                        this.schedule_expiry();
                    } else {
                        log::warn!("clipboard paste rejected before admission");
                    }
                }
                _ => log::warn!("clipboard paste rejected: native selection unavailable"),
            }
        })
        .detach();
    }

    fn schedule_expiry(self: &Arc<Self>) {
        let mut deadline = {
            let _commit = self.commit.lock().unwrap();
            let session = self.session.lock().unwrap();
            let deadline = match session.protocol.grant_deadline() {
                Some(deadline) => deadline,
                None => return,
            };
            if self.expiry_task.swap(true, Ordering::AcqRel) {
                return;
            }
            deadline
        };
        let weak = Arc::downgrade(self);
        promise::spawn::spawn(async move {
            loop {
                smol::Timer::at(deadline).await;
                let this = match weak.upgrade() {
                    Some(this) => this,
                    None => return,
                };
                // Grant replacement and task termination share the admission
                // lock, so a replacement cannot lose its expiry wakeup.
                let _commit = this.commit.lock().unwrap();
                this.reap();
                let session = this.session.lock().unwrap();
                match session.protocol.grant_deadline() {
                    Some(next) => deadline = next,
                    None => {
                        this.expiry_task.store(false, Ordering::Release);
                        return;
                    }
                }
            }
        })
        .detach();
    }
}

fn offer_validity(offer: Arc<dyn ClipboardOffer>) -> Arc<dyn Fn() -> bool + Send + Sync> {
    offer
        .validity_check()
        .unwrap_or_else(|| Arc::new(move || offer.valid()))
}

#[cfg(test)]
mod tests;

fn read_error(err: &anyhow::Error) -> Option<&'static str> {
    match err.downcast_ref::<ClipboardError>() {
        Some(ClipboardError::Unsupported) => Some("ENOSYS"),
        Some(ClipboardError::Stale | ClipboardError::Limit) => Some("EPERM"),
        Some(ClipboardError::Temporary) => Some("EBUSY"),
        None => None, // Protocol has no generic/permanent read I/O error.
    }
}

impl TermWindow {
    /// Attaches only native local panes; proxy/overlay panes retain legacy paste.
    pub(super) fn sync_mime_clipboards(&mut self) {
        let window = match self.window.clone() {
            Some(w) if w.supports_mime_clipboard() => w,
            _ => return,
        };
        let mux = Mux::get();
        let active = self.get_active_pane_or_overlay().map(|p| p.pane_id());
        if active != self.mime_active_pane {
            self.revoke_mime_clipboards();
            self.mime_active_pane = active;
        }
        let panes: Vec<_> = match mux.get_window(self.mux_window_id) {
            Some(w) => w
                .iter()
                .flat_map(|tab| tab.iter_panes_ignoring_zoom().into_iter().map(|p| p.pane))
                .collect(),
            None => vec![],
        };
        let mut present = HashSet::new();
        for pane in panes {
            if pane.downcast_ref::<mux::localpane::LocalPane>().is_none() {
                continue;
            }
            let id = pane.pane_id();
            present.insert(id);
            if let std::collections::hash_map::Entry::Vacant(entry) = self.mime_clipboards.entry(id)
            {
                let handler = Arc::new(MimeClipboard::new(window.clone(), id));
                if pane.set_mime_clipboard(handler.clone()) {
                    entry.insert(handler);
                }
            }
            pane.set_clipboard_byte_limit(self.config.osc5522_clipboard_read_limit);
        }
        self.mime_clipboards.retain(|id, handler| {
            if !present.contains(id) {
                handler.detach();
                return false;
            }
            handler.reap();
            true
        });
    }

    /// Handles one paste gesture when mode5522 supersedes bracketed text paste.
    pub(super) fn paste_mime_clipboard(&self, pane: &Arc<dyn Pane>, clipboard: Clipboard) -> bool {
        if let Some(handler) = self.mime_clipboards.get(&pane.pane_id()) {
            if handler.enabled() {
                handler.paste(self, clipboard);
                return true;
            }
        }
        false
    }

    /// Revokes GUI attachment authority without waiting for native I/O.
    pub(super) fn revoke_mime_clipboards(&self) {
        for handler in self.mime_clipboards.values() {
            handler.invalidate();
        }
    }

    /// Removes capability from endpoints retained by surviving/headless panes.
    pub(super) fn detach_mime_clipboards(&self) {
        for handler in self.mime_clipboards.values() {
            handler.detach();
        }
    }
}
