//! ASH, the UART framing layer of EZSP (Silicon Labs UG101).
//!
//! Only the pure frame codec lives here: bytes in, frames out, and back.
//! The session state machine (retransmission, windowing) sits on top of it.

mod codec;

pub use codec::{AshError, Decoder, Frame, crc16, randomize};
