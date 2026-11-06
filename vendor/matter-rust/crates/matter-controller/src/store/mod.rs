//! Persistence abstraction. The controller writes an opaque, versioned
//! snapshot blob through this trait; it never assumes a filesystem.

mod file;
pub use file::FileStore;

/// Errors a [`ControllerStore`] may return.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// An underlying I/O operation failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// A durable home for the controller's snapshot blob.
///
/// The controller owns the blob *format* (a versioned TLV encoding); the
/// store only moves opaque bytes. Implementors are responsible for
/// at-rest protection — the snapshot contains private keys in the clear.
pub trait ControllerStore: Send + Sync {
    /// Load the persisted snapshot, or `None` if nothing has been stored yet.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backing store cannot be read.
    fn load(&self) -> Result<Option<Vec<u8>>, StoreError>;

    /// Atomically replace the persisted snapshot.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] if the backing store cannot be written.
    fn save(&self, snapshot: &[u8]) -> Result<(), StoreError>;
}
