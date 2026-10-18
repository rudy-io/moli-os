//! ASH, the UART framing layer of EZSP (Silicon Labs UG101).
//!
//! The frame codec turns bytes into frames and back; the session state
//! machine on top of it numbers, acknowledges and retransmits them.

mod codec;
mod session;

pub use codec::{AshError, Decoder, Frame, crc16, randomize};
pub use session::{Busy, Event, Session};
