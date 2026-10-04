//! Native arbitrary-format clipboard interfaces, without filesystem authority.
use crate::Clipboard;
use promise::Future;
use std::collections::BTreeMap;
use std::sync::Arc;

/// Native MIME bytes; aliases may share the same allocation.
pub type ClipboardData = BTreeMap<String, Arc<Vec<u8>>>;

/// Native read failures with protocol-relevant distinctions.
#[derive(Debug, thiserror::Error)]
pub enum ClipboardError {
    /// The compositor does not provide this selection.
    #[error("clipboard location unsupported")]
    Unsupported,
    /// The retained offer no longer owns this selection.
    #[error("clipboard offer expired")]
    Stale,
    /// The configured aggregate resource policy denies the read.
    #[error("clipboard read denied by size policy")]
    Limit,
    /// A bounded acquisition could not complete in time.
    #[error("clipboard temporarily unavailable")]
    Temporary,
}

/// A specific native offer, not a reference to whichever clipboard is current.
pub trait ClipboardOffer: Send + Sync {
    /// Returns every native advertised format, without content acquisition.
    fn mime_types(&self) -> &[String];
    /// Checks native selection generation.
    fn valid(&self) -> bool;
    /// Acquires only requested formats under an aggregate decoded-byte cap.
    fn read(
        &self,
        types: Vec<String>,
        limit: usize,
        valid: Arc<dyn Fn() -> bool + Send + Sync>,
    ) -> Future<ClipboardData>;
}

/// Returns the clipboard location from the protocol's primary flag.
pub fn location(primary: bool) -> Clipboard {
    if primary {
        Clipboard::PrimarySelection
    } else {
        Clipboard::Clipboard
    }
}
