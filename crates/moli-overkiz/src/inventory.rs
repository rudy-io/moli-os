//! Bounded private cloud observations. This module performs no I/O or evaluation.
mod model;
mod parse;
pub use model::*;
pub use parse::*;
#[cfg(test)]
mod tests;

pub const INVENTORY_KEY: &str = "cloud_inventory_v1";
pub const MAX_CATALOG_BYTES: usize = 1_048_576;
pub const MAX_COLLECTION_BYTES: usize = 524_288;
pub const MAX_DEFINITION_BYTES: usize = 65_536;
pub const MAX_DEPTH: usize = 24;
pub const MAX_NODES: usize = 32_768;
pub const MAX_OBJECTS: usize = 2_048;
pub const MAX_STRING_BYTES: usize = 8_192;
pub const MAX_LABEL_BYTES: usize = 256;
pub const MAX_ID_BYTES: usize = 256;
