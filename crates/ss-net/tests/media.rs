use bytes::BytesMut;
use proptest::prelude::*;
use ss_net::media::{DatagramHeader, MAX_AUDIO_PAYLOAD, MAX_TRACK_ID, MediaError, StreamHeader};

fn header(track_id: u32) -> DatagramHeader {
    DatagramHeader {
        version: 0,
        kind: 0,
        flags: 3,
        track_id,
        seq: 0x1234,
        timestamp: 0x5678_9abc,
    }
}

proptest! {
    #[test]
    fn datagram_round_trip(track in 0..=MAX_TRACK_ID, seq in any::<u16>(), timestamp in any::<u32>(), flags in 0_u8..4, payload in prop::collection::vec(any::<u8>(), 0..=MAX_AUDIO_PAYLOAD)) {
        let original = DatagramHeader { seq, timestamp, flags, ..header(track) };
        let mut bytes = BytesMut::new();
        original.encode_into(&mut bytes).unwrap();
        let offset = bytes.len();
        bytes.extend_from_slice(&payload);
        let (parsed, rest) = DatagramHeader::parse(&bytes).unwrap();
        prop_assert_eq!(parsed, original);
        prop_assert_eq!(rest, payload.as_slice());
        prop_assert_eq!(rest.as_ptr(), bytes[offset..].as_ptr());
    }

    #[test]
    fn stream_round_trip(track in 0..=MAX_TRACK_ID, group_id in any::<u32>(), seq in any::<u16>(), timestamp in any::<u32>()) {
        let original = StreamHeader { media: DatagramHeader { kind: 1, seq, timestamp, ..header(track) }, group_id };
        let mut bytes = BytesMut::new();
        original.encode_into(&mut bytes).unwrap();
        bytes.extend_from_slice(b"opaque video");
        let (parsed, rest) = StreamHeader::parse(&bytes).unwrap();
        prop_assert_eq!(parsed, original);
        prop_assert_eq!(rest, b"opaque video");
    }

    #[test]
    fn arbitrary_input_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..2048)) {
        let _ = DatagramHeader::parse(&bytes);
        let _ = StreamHeader::parse(&bytes);
    }
}

#[test]
fn canonical_wire_bytes_and_boundaries() {
    for (id, varint) in [
        (0, vec![0]),
        (127, vec![127]),
        (128, vec![128, 1]),
        (16_383, vec![255, 127]),
        (16_384, vec![128, 128, 1]),
        (MAX_TRACK_ID, vec![255, 255, 127]),
    ] {
        let mut bytes = BytesMut::new();
        header(id).encode_into(&mut bytes).unwrap();
        let mut expected = vec![3];
        expected.extend(varint);
        expected.extend([0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert_eq!(bytes.as_ref(), expected);
        for end in 0..bytes.len() {
            assert!(DatagramHeader::parse(&bytes[..end]).is_err());
        }
    }
}

#[test]
fn invalid_values_and_encodings_are_rejected() {
    for (invalid, error) in [
        (
            DatagramHeader {
                version: 1,
                ..header(1)
            },
            MediaError::Version,
        ),
        (
            DatagramHeader {
                kind: 1,
                ..header(1)
            },
            MediaError::Kind,
        ),
        (
            DatagramHeader {
                flags: 4,
                ..header(1)
            },
            MediaError::Flags,
        ),
        (header(MAX_TRACK_ID + 1), MediaError::TrackId),
    ] {
        let mut bytes = BytesMut::from(&b"existing"[..]);
        assert_eq!(invalid.encode_into(&mut bytes), Err(error));
        assert_eq!(bytes.as_ref(), b"existing");
    }
    for bytes in [
        &[0, 128, 0, 0, 0, 0, 0, 0, 0][..],
        &[0, 128, 128, 128, 0, 0, 0, 0, 0, 0][..],
    ] {
        assert_eq!(DatagramHeader::parse(bytes), Err(MediaError::TrackId));
    }
    for (first, error) in [
        (0x40, MediaError::Version),
        (0x10, MediaError::Kind),
        (4, MediaError::Flags),
    ] {
        assert_eq!(
            DatagramHeader::parse(&[first, 1, 0, 0, 0, 0, 0, 0]),
            Err(error)
        );
    }
    let mut bytes = BytesMut::new();
    header(1).encode_into(&mut bytes).unwrap();
    bytes.resize(bytes.len() + MAX_AUDIO_PAYLOAD + 1, 0);
    assert_eq!(
        DatagramHeader::parse(&bytes),
        Err(MediaError::PayloadTooLarge)
    );
}
