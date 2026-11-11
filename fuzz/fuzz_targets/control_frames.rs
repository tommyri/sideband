#![no_main]

//! Protobuf decode of one control frame, in both directions. Every frame that
//! decodes must re-encode to a frame that decodes to the same message.
//!
//! Framing over a stream, with arbitrary chunk boundaries, is `control_stream`,
//! which drives the parser both peers run.

use libfuzzer_sys::fuzz_target;
use prost::Message;
use ss_proto::{
    MAX_FRAME_LEN, decode, encode,
    v1::{ClientEnvelope, ServerEnvelope},
};

fn round_trip<M: Message + Default + PartialEq + std::fmt::Debug>(message: &M) {
    let frame = encode(message).expect("a decoded message fits a frame");
    assert!(frame.len() <= MAX_FRAME_LEN + 4);
    let again: M = decode(&frame).expect("re-encoded frame decodes");
    assert_eq!(&again, message);
}

fuzz_target!(|data: &[u8]| {
    if let Ok(envelope) = decode::<ClientEnvelope>(data) {
        round_trip(&envelope);
    }
    if let Ok(envelope) = decode::<ServerEnvelope>(data) {
        round_trip(&envelope);
    }
    if let Ok(envelope) = ClientEnvelope::decode(data) {
        round_trip(&envelope);
    }
    if let Ok(envelope) = ServerEnvelope::decode(data) {
        round_trip(&envelope);
    }
});
