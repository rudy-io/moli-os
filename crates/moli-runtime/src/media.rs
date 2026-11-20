//! Images from devices (camera snapshots), documents to devices (printers).
//!
//! A driver that can produce an image for one of its devices registers a
//! [`SnapshotSource`] with [`crate::DriverCtx::provide_snapshots`]; the hub
//! calls it directly when a surface asks. No queue: an image nobody asked
//! for is never fetched. A printer registers a [`PrintSink`] the same way
//! ([`crate::DriverCtx::provide_printing`]). A device with live video a
//! browser plays itself (WebRTC) registers a [`LiveSource`]
//! ([`crate::DriverCtx::provide_live`]): Moli only passes the browser's
//! offer and the device's answer; the video goes from the device to the
//! browser, never through Moli.

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

/// Live video, played by the browser (WebRTC): answers the browser's offer
/// (SDP) with the device's own. Only the signalling passes through Moli.
/// `viewer` is the browser's address on the home network, when Moli knows
/// it: a browser hides its own behind a `.local` name a device may not
/// resolve.
pub trait LiveSource: std::fmt::Debug + Send + Sync + 'static {
    fn answer(
        &self,
        offer: String,
        viewer: Option<std::net::IpAddr>,
    ) -> BoxFuture<'_, anyhow::Result<String>>;
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum LiveError {
    #[error("unknown device")]
    UnknownDevice,
    #[error("this device has no live video")]
    NoLive,
    #[error("no answer in time")]
    Timeout,
    #[error("live video unavailable: {0}")]
    Failed(String),
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

// ---- review (an NVR's past: events, recordings, zones) ----------------------

/// Something the camera saw (an NVR's review item: an alert or a detection),
/// times in seconds since the epoch.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct ReviewItem {
    pub id: String,
    pub start: f64,
    /// `None` while it lasts.
    pub end: Option<f64>,
    /// `alert` or `detection`.
    pub severity: String,
    /// What was seen (`person`, `car`…).
    pub labels: Vec<String>,
    /// The zones it went through.
    pub zones: Vec<String>,
    /// The events behind it (their images and clips).
    pub events: Vec<String>,
}

/// A stretch the NVR recorded.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize)]
pub struct Recorded {
    pub start: f64,
    pub end: f64,
}

/// A zone drawn on the camera's picture: points from 0 to 1 (x right, y down).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Zone {
    pub name: String,
    pub points: Vec<(f64, f64)>,
}

/// One piece of the past to show.
#[derive(Clone, Debug, PartialEq)]
pub enum ReviewMedia {
    /// An event's picture (with its box).
    EventImage(String),
    /// An event's small picture.
    EventThumbnail(String),
    /// An event's clip (MP4).
    EventClip(String),
    /// What was recorded between two times (MP4).
    Recording { start: f64, end: f64 },
}

/// The NVR's answer, streamed (a clip can be large: never held in memory).
pub struct MediaReply {
    pub status: u16,
    /// The headers worth passing on (content type, length, range).
    pub headers: Vec<(String, String)>,
    pub body: futures::stream::BoxStream<'static, Result<bytes::Bytes, std::io::Error>>,
}

impl std::fmt::Debug for MediaReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MediaReply")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// A camera's past, from its NVR (Frigate): what it saw, what it recorded,
/// its zones, and their pictures and clips. Read only.
pub trait ReviewSource: std::fmt::Debug + Send + Sync + 'static {
    /// What it saw between `after` and `before` (seconds), newest first.
    fn review(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<ReviewItem>>>;
    /// What it recorded between `after` and `before`.
    fn recorded(&self, after: f64, before: f64) -> BoxFuture<'_, anyhow::Result<Vec<Recorded>>>;
    /// Its zones.
    fn zones(&self) -> BoxFuture<'_, anyhow::Result<Vec<Zone>>>;
    /// A picture or a clip; `range` is the browser's `Range` header (seeking).
    fn media(
        &self,
        what: ReviewMedia,
        range: Option<String>,
    ) -> BoxFuture<'_, anyhow::Result<MediaReply>>;
}
