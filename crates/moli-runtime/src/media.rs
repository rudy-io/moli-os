//! Images from devices (camera snapshots), documents to devices (printers).
//!
//! A driver that can produce an image for one of its devices registers a
//! [`SnapshotSource`] with [`crate::DriverCtx::provide_snapshots`]; the hub
//! calls it directly when a surface asks. No queue: an image nobody asked
//! for is never fetched. A printer registers a [`PrintSink`] the same way
//! ([`crate::DriverCtx::provide_printing`]).

use crate::BoxFuture;

/// One image, as the device produced it.
#[derive(Clone, PartialEq, Eq)]
pub struct Image {
    pub content_type: String,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for Image {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Image")
            .field("content_type", &self.content_type)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// Produces a fresh image on demand.
pub trait SnapshotSource: std::fmt::Debug + Send + Sync + 'static {
    fn snapshot(&self) -> BoxFuture<'_, anyhow::Result<Image>>;
}

/// A document to print (a page, a photo), in a format the printer takes.
#[derive(Clone, PartialEq, Eq)]
pub struct Document {
    /// What the person printed (a file name): shown while it prints.
    pub name: String,
    pub content_type: String,
    pub bytes: Vec<u8>,
    pub copies: u32,
    /// Colour, or black only.
    pub color: bool,
    /// Both sides of the sheet.
    pub two_sided: bool,
}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("name", &self.name)
            .field("content_type", &self.content_type)
            .field("bytes", &self.bytes.len())
            .field("copies", &self.copies)
            .field("color", &self.color)
            .field("two_sided", &self.two_sided)
            .finish()
    }
}

/// Takes documents to print. Answers once the document is queued (a page
/// takes a minute to print; nobody waits on it): its progress shows in the
/// printer's points.
pub trait PrintSink: std::fmt::Debug + Send + Sync + 'static {
    fn print(&self, document: Document) -> BoxFuture<'_, anyhow::Result<()>>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PrintError {
    #[error("unknown device")]
    UnknownDevice,
    #[error("this device does not print")]
    NotAPrinter,
    #[error("{0}")]
    Refused(String),
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SnapshotError {
    #[error("unknown device")]
    UnknownDevice,
    #[error("this device has no image")]
    NotACamera,
    #[error("no image in time")]
    Timeout,
    #[error("image unavailable: {0}")]
    Failed(String),
}
