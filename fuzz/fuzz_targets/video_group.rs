#![no_main]

//! The video group parser (`ss_net::media::stream::GroupParser`) behind
//! `read_header` and `GroupReader`, fed in arbitrary chunks.
//!
//! The first byte says how many of the following bytes are chunk lengths
//! (zero-length chunks included); the rest is one group stream. Chunked
//! parsing must match a single whole-stream feed, items must re-encode to
//! exactly the bytes they consumed, and every frame must obey the limits.

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use ss_net::media::stream::{GroupParser, KEYFRAME, MAX_FRAME_BYTES, Parsed};

/// How a group stream ended.
#[derive(Debug, PartialEq)]
enum End {
    /// Between frames, after at least one.
    Clean,
    /// Mid-item, or before the first frame.
    Truncated,
    /// The parser rejected the bytes.
    Invalid,
}

/// Items, bytes consumed, and how the stream ended.
fn run(cuts: &[u8], mut data: &[u8]) -> (Vec<Parsed>, usize, End) {
    let total = data.len();
    let mut parser = GroupParser::default();
    let mut items = Vec::new();
    let mut cut = cuts.iter().cycle();
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
            match parser.feed(&mut chunk) {
                Ok(Some(item)) => items.push(item),
                Ok(None) => {
                    assert!(chunk.is_empty(), "stopped short without an item");
                    assert!(parser.wanted() > 0);
                    break;
                }
                Err(_) => {
                    assert!(parser.feed(&mut &[][..]).is_err(), "error was not sticky");
                    assert!(parser.finish().is_err());
                    // The parser stops at the byte that failed.
                    return (items, total - data.len() - chunk.len(), End::Invalid);
                }
            }
            if chunk.is_empty() {
                break;
            }
        }
    }
    let end = if parser.finish().is_ok() {
        End::Clean
    } else {
        End::Truncated
    };
    (items, total, end)
}

fn encode(items: &[Parsed]) -> Vec<u8> {
    let mut bytes = BytesMut::new();
    for item in items {
        match item {
            Parsed::Header(header) => header.encode_into(&mut bytes).unwrap(),
            Parsed::Frame(frame) => {
                bytes.extend_from_slice(&u32::try_from(frame.payload.len()).unwrap().to_be_bytes());
                bytes.extend_from_slice(&frame.timestamp.to_be_bytes());
                bytes.extend_from_slice(&[u8::from(frame.keyframe) * KEYFRAME]);
                bytes.extend_from_slice(&frame.payload);
            }
        }
    }
    bytes.to_vec()
}

fuzz_target!(|data: &[u8]| {
    let Some((&count, rest)) = data.split_first() else {
        return;
    };
    let (cuts, stream) = rest.split_at(usize::from(count % 32).min(rest.len()));
    let (items, consumed, end) = run(cuts, stream);
    let (whole, whole_consumed, whole_end) = run(&[], stream);
    assert_eq!(items, whole);
    assert_eq!(end, whole_end);
    assert_eq!(consumed, whole_consumed);
    for (index, item) in items.iter().enumerate() {
        match item {
            Parsed::Header(_) => assert_eq!(index, 0, "a second header"),
            Parsed::Frame(frame) => {
                assert!(index > 0, "a frame before the header");
                assert!(!frame.payload.is_empty() && frame.payload.len() <= MAX_FRAME_BYTES);
                assert!(
                    index > 1 || frame.keyframe,
                    "the group starts with a delta frame"
                );
            }
        }
    }
    let encoded = encode(&items);
    assert_eq!(
        encoded[..],
        stream[..encoded.len()],
        "items do not re-encode to their bytes"
    );
    if end == End::Clean {
        assert_eq!(encoded.len(), stream.len());
    }
});
