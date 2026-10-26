#![no_main]

use bytes::BytesMut;
use libfuzzer_sys::fuzz_target;
use ss_net::media::{DatagramHeader, StreamHeader};

fuzz_target!(|data: &[u8]| {
    if let Ok((header, payload)) = DatagramHeader::parse(data) {
        let mut encoded = BytesMut::new();
        header.encode_into(&mut encoded).unwrap();
        encoded.extend_from_slice(payload);
        assert_eq!(encoded.as_ref(), data);
    }
    if let Ok((header, payload)) = StreamHeader::parse(data) {
        let mut encoded = BytesMut::new();
        header.encode_into(&mut encoded).unwrap();
        encoded.extend_from_slice(payload);
        assert_eq!(encoded.as_ref(), data);
    }
});
