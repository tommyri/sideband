#![no_main]

//! The control-stream parser both peers run (`ss_net::control::FrameDecoder`),
//! fed in arbitrary chunks, in both directions.
//!
//! The first byte says how many of the following bytes are chunk lengths
//! (zero-length chunks included); the rest is the stream. Chunked parsing
//! must produce the same messages and the same outcome as a plain reference
//! parse of the whole stream, never consume past a frame, and every message
//! must survive a re-encode.

use libfuzzer_sys::fuzz_target;
use prost::Message;
use ss_net::control::FrameDecoder;
use ss_proto::{
    FrameError, MAX_FRAME_LEN, decode, encode,
    v1::{ClientEnvelope, ServerEnvelope},
};

/// How a stream ended: cleanly between frames, mid-frame, or on an error.
#[derive(Debug, PartialEq)]
enum End {
    Boundary,
    Partial,
    TooLarge(usize),
    Invalid,
}

/// Whole-buffer reference: length prefix, bound, then one-shot decode.
fn reference<M: Message + Default>(mut data: &[u8]) -> (Vec<M>, End) {
    let mut messages = Vec::new();
    loop {
        if data.is_empty() {
            return (messages, End::Boundary);
        }
        let Some(prefix) = data.get(..4) else {
            return (messages, End::Partial);
        };
        let len = u32::from_be_bytes(prefix.try_into().unwrap()) as usize;
        if len > MAX_FRAME_LEN {
            return (messages, End::TooLarge(len));
        }
        let Some(frame) = data.get(..4 + len) else {
            return (messages, End::Partial);
        };
        match decode::<M>(frame) {
            Ok(message) => messages.push(message),
            Err(_) => return (messages, End::Invalid),
        }
        data = &data[4 + len..];
    }
}

fn chunked<M: Message + Default>(cuts: &[u8], mut data: &[u8]) -> (Vec<M>, End) {
    let mut decoder = FrameDecoder::<M>::default();
    let mut messages = Vec::new();
    let mut cut = cuts.iter().cycle();
    let mut in_frame = false;
    while !data.is_empty() {
        // Zero-length chunks are fed too; all-zero cuts would never finish.
        let size = if cuts.iter().all(|&size| size == 0) {
            data.len()
        } else {
            usize::from(*cut.next().unwrap())
        };
        let (mut chunk, rest) = data.split_at(size.min(data.len()));
        data = rest;
        loop {
            let before = chunk.len();
            match decoder.feed(&mut chunk) {
                Ok(Some(message)) => {
                    messages.push(message);
                    in_frame = false;
                }
                Ok(None) => {
                    assert!(chunk.is_empty(), "stopped short without a frame");
                    in_frame |= before > 0;
                    assert!(decoder.wanted().unwrap() > 0);
                    break;
                }
                Err(FrameError::TooLarge { len }) => {
                    assert!(len > MAX_FRAME_LEN);
                    assert!(decoder.feed(&mut &[][..]).is_err(), "error was not sticky");
                    return (messages, End::TooLarge(len));
                }
                Err(_) => {
                    assert!(decoder.feed(&mut &[][..]).is_err(), "error was not sticky");
                    return (messages, End::Invalid);
                }
            }
            if chunk.is_empty() {
                break;
            }
        }
    }
    (
        messages,
        if in_frame {
            End::Partial
        } else {
            End::Boundary
        },
    )
}

fn check<M: Message + Default + PartialEq + std::fmt::Debug>(cuts: &[u8], data: &[u8]) {
    let (got, end) = chunked::<M>(cuts, data);
    let (want, want_end) = reference::<M>(data);
    assert_eq!(got, want);
    assert_eq!(end, want_end);
    for message in &got {
        let frame = encode(message).expect("a decoded message fits a frame");
        assert_eq!(
            &decode::<M>(&frame).expect("re-encoded frame decodes"),
            message
        );
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&count, rest)) = data.split_first() else {
        return;
    };
    let (cuts, stream) = rest.split_at(usize::from(count % 32).min(rest.len()));
    check::<ClientEnvelope>(cuts, stream);
    check::<ServerEnvelope>(cuts, stream);
});
