//! The household's people, as the conversation may touch them: add one (a name
//! and nothing else: no e-mail, no password, no role above member) and take
//! back an addition nobody built on. The people live in the API's store; it
//! hands Moli this narrow door.

use std::future::Future;
use std::pin::Pin;

pub type Done<'a, T> = Pin<Box<dyn Future<Output = Result<T, String>> + Send + 'a>>;

pub trait Directory: Send + Sync + std::fmt::Debug {
    /// Who lives here, by name.
    fn names(&self) -> Vec<String>;
    /// A new member; the id they get.
    fn add<'a>(&'a self, name: &'a str) -> Done<'a, String>;
    /// Removes a person added so (same id and name as when added), while nothing was
    /// set up for them (no e-mail, password, device, colour or invitation): otherwise
    /// it is refused, and the household does it from the People screen.
    fn remove_unused<'a>(&'a self, id: &'a str, name: &'a str) -> Done<'a, ()>;
}
