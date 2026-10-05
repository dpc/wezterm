//! OSC5522 wire state and bounded, cancellable response serialization.
//!
//! Native clipboard access and user permission decisions belong to the GUI.
#[cfg(test)]
mod tests;
use base64::{engine::general_purpose::STANDARD, Engine};
use std::collections::BTreeMap;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Maximum accepted decoded write size (independent of a client's artifact limit).
pub const WRITE_LIMIT: usize = 64 * 1024 * 1024;
/// Maximum metadata/list size; native offers beyond this cannot be represented.
pub const METADATA_LIMIT: usize = 64 * 1024;
/// Lifetime of gesture authority and idle write assembly.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Native data indexed by exact MIME name; aliases share storage.
pub type ClipboardData = BTreeMap<String, Arc<Vec<u8>>>;

/// The GUI attachment through which a local terminal accesses OSC5522.
pub trait ClipboardHandler: Send + Sync {
    /// Reports whether the owning native GUI attachment still exists.
    fn supported(&self) -> bool;
    /// Permanently detaches this endpoint without affecting a newer attachment.
    fn detach(&self);
    /// Returns whether automatic paste notifications are enabled.
    fn enabled(&self) -> bool;
    /// Changes the mode, revoking authority when disabling it.
    fn set_enabled(&self, enabled: bool);
    /// Revokes all grants and in-progress operations.
    fn invalidate(&self);
    /// Dispatches one redacted parser packet to the GUI.
    fn handle(&self, fields: Vec<Vec<u8>>);
}

/// One response streamed as complete frames, with generation-based cancellation.
pub struct ClipboardResponse {
    /// Read or write response.
    pub kind: &'static str,
    /// Sanitized request correlation id, absent for unsolicited paste offers.
    pub id: String,
    /// Initial error status, or None for successful data followed by DONE.
    pub error: Option<&'static str>,
    /// Ordered data to serialize without creating a second base64-sized copy.
    pub data: ClipboardData,
    /// Primary-selection offer marker.
    pub primary: bool,
    /// Encoded one-use password, only on unsolicited offer OK.
    pub password: Option<String>,
    /// Attachment lifecycle generation.
    pub generation: Arc<AtomicU64>,
    /// Generation at admission.
    pub epoch: u64,
    /// Remains busy until serialization completes or is canceled.
    pub busy: Option<Arc<AtomicBool>>,
    /// Optional native ownership check at each complete-frame boundary.
    pub valid: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
}

impl Drop for ClipboardResponse {
    fn drop(&mut self) {
        if let Some(busy) = &self.busy {
            busy.store(false, Ordering::Release);
        }
    }
}

impl ClipboardResponse {
    /// Serializes at most one frame after a concurrent revocation.
    ///
    /// A started frame is completed with write_all; already-written bytes cannot
    /// be recalled. Dropping a PTY may truncate that transport, not splice it.
    pub fn write_to(&self, writer: &mut dyn Write) -> io::Result<()> {
        let id = if self.id.is_empty() {
            String::new()
        } else {
            format!(":id={}", self.id)
        };
        let prefix = format!("\x1b]5522;type={}{}", self.kind, id);
        let emit = |writer: &mut dyn Write, frame: &str| -> io::Result<bool> {
            if self.generation.load(Ordering::Acquire) != self.epoch
                || self.valid.as_ref().is_some_and(|valid| !valid())
            {
                return Ok(false);
            }
            writer.write_all(frame.as_bytes())?;
            Ok(true)
        };
        if let Some(error) = self.error {
            emit(writer, &format!("{prefix}:status={error}\x1b\\"))?;
            return writer.flush();
        }
        if self.kind == "read" {
            let loc = if self.primary { ":loc=primary" } else { "" };
            let pw = self
                .password
                .as_ref()
                .map(|p| format!(":pw={p}"))
                .unwrap_or_default();
            if !emit(writer, &format!("{prefix}:status=OK{loc}{pw}\x1b\\"))? {
                return Ok(());
            }
            for (mime, bytes) in &self.data {
                let mime = STANDARD.encode(mime);
                // An empty format is data too, not a missing format.
                if bytes.is_empty()
                    && !emit(writer, &format!("{prefix}:status=DATA:mime={mime};\x1b\\"))?
                {
                    return Ok(());
                }
                for chunk in bytes.chunks(4096) {
                    if !emit(
                        writer,
                        &format!(
                            "{prefix}:status=DATA:mime={mime};{}\x1b\\",
                            STANDARD.encode(chunk)
                        ),
                    )? {
                        return Ok(());
                    }
                }
            }
        }
        emit(writer, &format!("{prefix}:status=DONE\x1b\\"))?;
        writer.flush()
    }
}

/// A validated native operation, or a response requiring no native access.
pub enum ClipboardAction {
    /// Obtain current MIME names without granting access to content.
    List {
        id: String,
        primary: bool,
        busy: Arc<AtomicBool>,
    },
    /// Read selected formats from the retained gesture offer.
    Read {
        id: String,
        primary: bool,
        types: Vec<String>,
        retained: bool,
        busy: Arc<AtomicBool>,
    },
    /// Atomically replace native selection after complete validation.
    Write {
        id: String,
        primary: bool,
        data: ClipboardData,
        busy: Arc<AtomicBool>,
    },
    /// Send a response through the terminal's serialized writer.
    Reply(ClipboardResponse),
}

/// One paste grant, never transferable across native offers or locations.
struct Grant {
    /// Base64 password as emitted.
    password: String,
    /// Exact native MIME names.
    types: Vec<String>,
    /// Selection location.
    primary: bool,
    /// Absolute expiry.
    expires: Instant,
}

/// An atomic multi-format write under construction.
struct PendingWrite {
    /// Initial request id; continuations cannot change it.
    id: String,
    /// Target selection.
    primary: bool,
    /// Completed formats.
    data: ClipboardData,
    /// Current format, if any.
    mime: Option<String>,
    /// Current decoded bytes.
    bytes: Vec<u8>,
    /// Base64 suffix shorter than four characters.
    tail: Vec<u8>,
    /// Whether padding ended the current format.
    padded: bool,
    /// Total unique decoded bytes.
    size: usize,
    /// Aliases resolved only after all formats arrive.
    aliases: BTreeMap<String, String>,
    /// Idle expiry.
    expires: Instant,
}

impl PendingWrite {
    fn finish_type(&mut self) -> Result<(), &'static str> {
        if !self.tail.is_empty() {
            return Err("EINVAL");
        }
        if let Some(mime) = self.mime.take() {
            self.data
                .insert(mime, Arc::new(std::mem::take(&mut self.bytes)));
        }
        self.padded = false;
        Ok(())
    }

    fn append(&mut self, mime: String, payload: &[u8]) -> Result<(), &'static str> {
        if self.mime.as_ref() != Some(&mime) {
            self.finish_type()?;
            if self.data.contains_key(&mime) || self.data.len() >= 1024 {
                return Err("EINVAL");
            }
            self.mime = Some(mime);
        }
        for &byte in payload {
            if self.padded {
                return Err("EINVAL");
            }
            self.tail.push(byte);
            if self.tail.len() == 4 {
                let decoded = STANDARD.decode(&self.tail).map_err(|_| "EINVAL")?;
                self.padded = self.tail.contains(&b'=');
                self.size = self.size.checked_add(decoded.len()).ok_or("EFBIG")?;
                if self.size > WRITE_LIMIT {
                    return Err("EFBIG");
                }
                self.bytes.extend(decoded);
                self.tail.clear();
            }
        }
        Ok(())
    }
}

/// Pure protocol state for one GUI attachment; never logs secrets or payloads.
pub struct ClipboardSession {
    /// Shared epoch, incremented synchronously by terminal reset/focus hooks.
    pub generation: Arc<AtomicU64>,
    /// Epoch represented by stored state.
    epoch: u64,
    /// Unused paste authority.
    grant: Option<Grant>,
    /// Current native operation or response stream.
    busy: Option<Arc<AtomicBool>>,
    /// Incomplete atomic write.
    write: Option<PendingWrite>,
}

impl ClipboardSession {
    /// Creates an empty attachment.
    pub fn new(generation: Arc<AtomicU64>) -> Self {
        let epoch = generation.load(Ordering::Acquire);
        Self {
            generation,
            epoch,
            grant: None,
            busy: None,
            write: None,
        }
    }

    /// Drops stale grants and partial writes without affecting newer operations.
    pub fn expire(&mut self, now: Instant) {
        let epoch = self.generation.load(Ordering::Acquire);
        if epoch != self.epoch {
            self.grant = None;
            self.write = None;
            self.busy = None;
            self.epoch = epoch;
        }
        if self.grant.as_ref().is_some_and(|g| now >= g.expires) {
            self.grant = None;
        }
        if self.write.as_ref().is_some_and(|w| now >= w.expires) {
            self.write = None;
        }
    }

    /// Indicates that a gesture must be rejected before paste-event admission.
    pub fn is_busy(&self) -> bool {
        self.write.is_some()
            || self
                .busy
                .as_ref()
                .is_some_and(|b| b.load(Ordering::Acquire))
    }

    /// Constructs a generation-bound response.
    pub fn response(
        &self,
        kind: &'static str,
        id: String,
        error: Option<&'static str>,
    ) -> ClipboardResponse {
        ClipboardResponse {
            kind,
            id,
            error,
            data: BTreeMap::new(),
            primary: false,
            password: None,
            generation: self.generation.clone(),
            epoch: self.epoch,
            busy: None,
            valid: None,
        }
    }

    /// Issues one grant and an unfiltered MIME offer for an accepted gesture.
    pub fn offer(
        &mut self,
        password: String,
        types: Vec<String>,
        primary: bool,
        now: Instant,
    ) -> Option<ClipboardResponse> {
        self.offer_for_epoch(
            password,
            types,
            primary,
            now,
            self.generation.load(Ordering::Acquire),
        )
    }

    /// Admits an asynchronous gesture only in its original attachment generation.
    pub fn offer_for_epoch(
        &mut self,
        password: String,
        types: Vec<String>,
        primary: bool,
        now: Instant,
        expected_epoch: u64,
    ) -> Option<ClipboardResponse> {
        self.expire(now);
        if self.epoch != expected_epoch || self.generation.load(Ordering::Acquire) != expected_epoch
        {
            return None;
        }
        if self.is_busy()
            || types.iter().any(|m| !valid_mime(m))
            || types.join(" ").len() > METADATA_LIMIT
        {
            return None;
        }
        self.grant = Some(Grant {
            password: password.clone(),
            types: types.clone(),
            primary,
            expires: now + TIMEOUT,
        });
        let mut reply = self.response("read", String::new(), None);
        reply.epoch = expected_epoch;
        reply.password = Some(password);
        reply.primary = primary;
        reply.busy = Some(self.start_busy());
        reply
            .data
            .insert(".".to_owned(), Arc::new(types.join(" ").into_bytes()));
        Some(reply)
    }

    fn start_busy(&mut self) -> Arc<AtomicBool> {
        let busy = Arc::new(AtomicBool::new(true));
        self.busy = Some(busy.clone());
        busy
    }

    /// Processes a complete OSC body; malformed reads are ignored per protocol.
    pub fn handle(
        &mut self,
        fields: &[Vec<u8>],
        reads_allowed: bool,
        writes_allowed: bool,
        now: Instant,
    ) -> Option<ClipboardAction> {
        self.handle_for_epoch(
            fields,
            reads_allowed,
            writes_allowed,
            now,
            self.generation.load(Ordering::Acquire),
        )
    }

    /// Rejects stale queued input before it can mutate a newer transaction.
    pub fn handle_for_epoch(
        &mut self,
        fields: &[Vec<u8>],
        reads_allowed: bool,
        writes_allowed: bool,
        now: Instant,
        expected_epoch: u64,
    ) -> Option<ClipboardAction> {
        self.expire(now);
        if self.epoch != expected_epoch || self.generation.load(Ordering::Acquire) != expected_epoch
        {
            return None;
        }
        let meta = fields.first()?;
        if meta.len() > METADATA_LIMIT || fields.len() > 2 {
            return self.malformed(fields);
        }
        let meta = match std::str::from_utf8(meta) {
            Ok(meta) => meta,
            Err(_) => return self.malformed(fields),
        };
        let mut values = BTreeMap::new();
        for item in meta.split(':') {
            let (key, value) = match item.split_once('=') {
                Some(pair) => pair,
                None => return self.malformed(fields),
            };
            if values.insert(key, value).is_some() {
                return self.malformed(fields);
            }
        }
        let kind = *values.get("type")?;
        let id: String = values
            .get("id")
            .unwrap_or(&"")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || "-_+.".contains(*c))
            .collect();
        if !matches!(kind, "read" | "write" | "wdata" | "walias") {
            return None;
        }
        if matches!(kind, "wdata" | "walias") {
            let write = self.write.as_ref()?; // drain before any continuation errors
            if !id.is_empty() && id != write.id {
                return Some(ClipboardAction::Reply(self.response(
                    "write",
                    id,
                    Some("EBUSY"),
                )));
            }
        }
        if kind == "write" && self.is_busy() {
            return Some(ClipboardAction::Reply(self.response(
                "write",
                id,
                Some("EBUSY"),
            )));
        }
        let primary = match values.get("loc").copied() {
            None => false,
            Some("primary") => true,
            _ => {
                let id = if matches!(kind, "wdata" | "walias") {
                    self.write.take().unwrap().id
                } else {
                    id
                };
                return Some(ClipboardAction::Reply(self.response(
                    if kind == "read" { "read" } else { "write" },
                    id,
                    Some("ENOSYS"),
                )));
            }
        };
        let payload = fields.get(1).map(Vec::as_slice).unwrap_or_default();
        for key in ["pw", "name"] {
            if let Some(value) = values.get(key) {
                if STANDARD
                    .decode(value)
                    .ok()
                    .and_then(|b| String::from_utf8(b).ok())
                    .is_none()
                {
                    return if kind == "read" {
                        None
                    } else {
                        self.malformed(fields)
                    };
                }
            }
        }
        if kind == "read" {
            if fields.len() != 2 || payload.len() > METADATA_LIMIT {
                return None;
            }
            let requested = decode_list(payload)?;
            if requested.is_empty() {
                return None;
            }
            if self.is_busy() {
                return Some(ClipboardAction::Reply(self.response(
                    "read",
                    id,
                    Some("EBUSY"),
                )));
            }
            if requested == ["."] {
                let busy = self.start_busy();
                return Some(ClipboardAction::List { id, primary, busy });
            }
            let name = values
                .get("name")
                .and_then(|v| STANDARD.decode(v).ok())
                .and_then(|v| String::from_utf8(v).ok());
            let password = values.get("pw").copied();
            let grant = self.grant.as_ref();
            let authorized = name.as_ref().is_some_and(|n| !n.is_empty())
                && grant
                    .is_some_and(|g| Some(g.password.as_str()) == password && g.primary == primary);
            if !authorized && !reads_allowed {
                return Some(ClipboardAction::Reply(self.response(
                    "read",
                    id,
                    Some("EPERM"),
                )));
            }
            let types: Vec<_> = requested
                .into_iter()
                .filter(|m| !authorized || grant.unwrap().types.contains(m))
                .collect();
            // No available types is a successful empty read, not ENOSYS (location).
            if authorized && !types.is_empty() {
                self.grant = None;
            }
            let busy = self.start_busy();
            return Some(ClipboardAction::Read {
                id,
                primary,
                types,
                retained: authorized,
                busy,
            });
        }
        if kind == "write" {
            // A fresh write resets drain state, but cannot steal an active transaction.
            if self.is_busy() {
                return Some(ClipboardAction::Reply(self.response(
                    "write",
                    id,
                    Some("EBUSY"),
                )));
            }
            if !writes_allowed {
                self.write = None;
                return Some(ClipboardAction::Reply(self.response(
                    "write",
                    id,
                    Some("EPERM"),
                )));
            }
            if !payload.is_empty() || fields.len() > 2 {
                return Some(ClipboardAction::Reply(self.response(
                    "write",
                    id,
                    Some("EINVAL"),
                )));
            }
            self.write = Some(PendingWrite {
                id,
                primary,
                data: BTreeMap::new(),
                mime: None,
                bytes: Vec::new(),
                tail: Vec::new(),
                padded: false,
                size: 0,
                aliases: BTreeMap::new(),
                expires: now + TIMEOUT,
            });
            return None;
        }
        if kind != "wdata" && kind != "walias" {
            return None;
        }
        let mut write = self.write.take()?; // absent means drain after failure/reset
        if !writes_allowed {
            return Some(ClipboardAction::Reply(self.response(
                "write",
                write.id,
                Some("EPERM"),
            )));
        }
        if !id.is_empty() && id != write.id {
            let reply = self.response("write", id, Some("EBUSY"));
            self.write = Some(write);
            return Some(ClipboardAction::Reply(reply));
        }
        let result = (|| -> Result<bool, &'static str> {
            if fields.len() > 2 || payload.len() > METADATA_LIMIT {
                return Err("EINVAL");
            }
            let mime = values
                .get("mime")
                .map(|v| decode_mime(v.as_bytes()).ok_or("EINVAL"))
                .transpose()?;
            if kind == "wdata" {
                match mime {
                    Some(mime) => write.append(mime, payload)?,
                    None if payload.is_empty() => {
                        write.finish_type()?;
                        for (alias, target) in &write.aliases {
                            let data = write.data.get(target).ok_or("EINVAL")?.clone();
                            if write.data.contains_key(alias) {
                                return Err("EINVAL");
                            }
                            write.data.insert(alias.clone(), data);
                        }
                        return Ok(true);
                    }
                    None => return Err("EINVAL"),
                }
            } else {
                let target = mime.ok_or("EINVAL")?;
                let aliases = decode_list(payload).ok_or("EINVAL")?;
                for alias in aliases {
                    if !valid_mime(&alias)
                        || write.aliases.len() >= 1024
                        || write.aliases.insert(alias, target.clone()).is_some()
                    {
                        return Err("EINVAL");
                    }
                }
            }
            Ok(false)
        })();
        match result {
            Err(error) => Some(ClipboardAction::Reply(self.response(
                "write",
                write.id,
                Some(error),
            ))),
            Ok(true) => {
                let busy = self.start_busy();
                Some(ClipboardAction::Write {
                    id: write.id,
                    primary: write.primary,
                    data: write.data,
                    busy,
                })
            }
            Ok(false) => {
                write.expires = now + TIMEOUT;
                self.write = Some(write);
                None
            }
        }
    }

    /// Revokes gesture authority when its native offer changes before admission.
    pub fn revoke_grant(&mut self) {
        self.grant = None;
    }

    /// Reports whether native gesture identity still needs retaining.
    pub fn has_grant(&self) -> bool {
        self.grant.is_some()
    }

    /// Returns the current grant's exact expiry for deadline-driven retention cleanup.
    pub fn grant_deadline(&self) -> Option<Instant> {
        self.grant.as_ref().map(|g| g.expires)
    }

    fn malformed(&mut self, fields: &[Vec<u8>]) -> Option<ClipboardAction> {
        let meta = fields.first()?;
        let kind = meta
            .split(|b| *b == b':')
            .find_map(|p| p.strip_prefix(b"type="))?;
        let id: String = meta
            .split(|b| *b == b':')
            .find_map(|p| p.strip_prefix(b"id="))
            .unwrap_or_default()
            .iter()
            .copied()
            .filter(|b| b.is_ascii_alphanumeric() || b"-_+.".contains(b))
            .map(char::from)
            .collect();
        if kind == b"write" {
            return Some(ClipboardAction::Reply(self.response(
                "write",
                id,
                Some(if self.is_busy() { "EBUSY" } else { "EINVAL" }),
            )));
        }
        if kind != b"wdata" && kind != b"walias" {
            return None;
        }
        let write = self.write.as_ref()?;
        if !id.is_empty() && id != write.id {
            return Some(ClipboardAction::Reply(self.response(
                "write",
                id,
                Some("EBUSY"),
            )));
        }
        let id = self.write.take().unwrap().id;
        Some(ClipboardAction::Reply(self.response(
            "write",
            id,
            Some("EINVAL"),
        )))
    }
}

fn valid_mime(mime: &str) -> bool {
    !mime.is_empty() && mime.len() <= 4096 && mime.bytes().all(|b| b > b' ' && b < 127)
}

fn decode_mime(value: &[u8]) -> Option<String> {
    let value = String::from_utf8(STANDARD.decode(value).ok()?).ok()?;
    valid_mime(&value).then_some(value)
}

fn decode_list(value: &[u8]) -> Option<Vec<String>> {
    let decoded = STANDARD.decode(value).ok()?;
    let text = std::str::from_utf8(&decoded).ok()?;
    let types: Vec<_> = text.split_ascii_whitespace().map(str::to_owned).collect();
    (types.len() <= 1024 && types.iter().all(|m| valid_mime(m))).then_some(types)
}
