#![no_main]

//! Control-stream framing plus protobuf decode, in both directions.
//!
//! The first byte picks a read chunk size so partial reads are exercised the
//! way a QUIC stream delivers them. Every frame that decodes must re-encode
//! to a frame that decodes to the same message.

use std::{
    future::Future,
    io,
    pin::{Pin, pin},
    task::{Context, Poll, Waker},
};

use libfuzzer_sys::fuzz_target;
use prost::Message;
use ss_proto::{
    FrameError, MAX_FRAME_LEN, decode, encode, read_frame,
    v1::{ClientEnvelope, ServerEnvelope},
};
use tokio::io::{AsyncRead, ReadBuf};

/// An in-memory stream that hands out at most `chunk` bytes per read.
struct Chunked<'a> {
    data: &'a [u8],
    chunk: usize,
}

impl AsyncRead for Chunked<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let take = self.chunk.min(self.data.len()).min(buf.remaining());
        let (head, tail) = self.data.split_at(take);
        buf.put_slice(head);
        self.data = tail;
        Poll::Ready(Ok(()))
    }
}

/// `Chunked` never returns `Pending`, so one poll finishes the read.
fn ready<T>(future: impl Future<Output = T>) -> T {
    let mut future = pin!(future);
    match future.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("in-memory read returned Pending"),
    }
}

fn round_trip<M: Message + Default + PartialEq + std::fmt::Debug>(message: &M) {
    let frame = encode(message).expect("a decoded message fits a frame");
    assert!(frame.len() <= MAX_FRAME_LEN + 4);
    let again: M = decode(&frame).expect("re-encoded frame decodes");
    assert_eq!(&again, message);
}

fn stream<M: Message + Default + PartialEq + std::fmt::Debug>(data: &[u8], chunk: usize) {
    let mut reader = Chunked { data, chunk };
    // A frame of at least 4 bytes is consumed per success, so this ends.
    for _ in 0..=data.len() / 4 {
        match ready(read_frame::<_, M>(&mut reader)) {
            Ok(message) => round_trip(&message),
            Err(FrameError::TooLarge { len }) => {
                assert!(len > MAX_FRAME_LEN);
                break;
            }
            Err(_) => break,
        }
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&selector, rest)) = data.split_first() else {
        return;
    };
    let chunk = usize::from(selector % 16) + 1;
    if let Ok(envelope) = decode::<ClientEnvelope>(rest) {
        round_trip(&envelope);
    }
    if let Ok(envelope) = decode::<ServerEnvelope>(rest) {
        round_trip(&envelope);
    }
    if let Ok(envelope) = ClientEnvelope::decode(rest) {
        round_trip(&envelope);
    }
    stream::<ClientEnvelope>(rest, chunk);
    stream::<ServerEnvelope>(rest, chunk);
});
