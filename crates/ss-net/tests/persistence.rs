use ss_net::{
    Error,
    pins::{JsonPinStore, MemoryPinStore, PinStore, ServerId},
    tls::{self, fingerprint},
};
use std::sync::Arc;

fn exercise(store: &dyn PinStore) {
    let server = ServerId::new("EXAMPLE.COM.", 443).unwrap();
    let same = ServerId::new("example.com", 443).unwrap();
    let other = ServerId::new("example.com", 444).unwrap();
    assert_eq!(server, same);
    assert_eq!(store.get(&server).unwrap(), None);
    let first = fingerprint(b"first cert");
    let second = fingerprint(b"second cert");
    store.set(&server, first).unwrap();
    assert_eq!(store.get(&same).unwrap(), Some(first));
    assert_eq!(store.get(&other).unwrap(), None);
    assert!(
        matches!(store.remember(&server, second), Err(Error::FingerprintChanged { expected, received }) if expected == first && received == second)
    );
    assert_eq!(store.get(&server).unwrap(), Some(first));
    store.set(&server, second).unwrap();
    assert_eq!(store.get(&server).unwrap(), Some(second));
}

#[test]
fn pin_stores_round_trip_and_preserve_authority_boundaries() {
    exercise(&MemoryPinStore::default());
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pins.json");
    exercise(&JsonPinStore::open(&path).unwrap());
    let restored = JsonPinStore::open(&path).unwrap();
    assert_eq!(
        restored
            .get(&ServerId::new("example.com", 443).unwrap())
            .unwrap(),
        Some(fingerprint(b"second cert"))
    );
    assert_eq!(
        ServerId::new("[::1]", 443).unwrap(),
        ServerId::new("0:0:0:0:0:0:0:1", 443).unwrap()
    );
    std::fs::write(&path, "not json").unwrap();
    assert!(JsonPinStore::open(&path).is_err());
}

#[test]
fn failed_persistence_does_not_accept_a_pin() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pins.json");
    let store = JsonPinStore::open(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    let server = ServerId::new("localhost", 443).unwrap();
    assert!(store.set(&server, fingerprint(b"test")).is_err());
    assert_eq!(store.get(&server).unwrap(), None);
}

#[test]
fn concurrent_remember_cannot_replace_a_pin() {
    let store = Arc::new(MemoryPinStore::default());
    let server = ServerId::new("localhost", 443).unwrap();
    let tasks: Vec<_> = (0..8)
        .map(|byte| {
            let store = store.clone();
            let server = server.clone();
            std::thread::spawn(move || store.remember(&server, fingerprint(&[byte])))
        })
        .collect();
    assert_eq!(
        tasks
            .into_iter()
            .filter_map(|task| task.join().unwrap().ok())
            .count(),
        1
    );
}

#[test]
fn fingerprint_has_known_sha256_and_validated_json() {
    let value = fingerprint(b"abc");
    assert_eq!(
        value.to_string(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        serde_json::from_str::<tls::Fingerprint>(&serde_json::to_string(&value).unwrap()).unwrap(),
        value
    );
    for value in ["short".to_owned(), "x".repeat(64), "é".repeat(32)] {
        assert!(serde_json::from_str::<tls::Fingerprint>(&format!("\"{value}\"")).is_err());
    }
}

#[tokio::test]
async fn identity_is_persisted_atomically_and_never_silently_rotates() {
    let directory = tempfile::tempdir().unwrap();
    let (first, second) = tokio::join!(
        tls::generate_self_signed(directory.path()),
        tls::generate_self_signed(directory.path())
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.certificates, second.certificates);
    assert_eq!(
        first.private_key.secret_der(),
        second.private_key.secret_der()
    );
    let path = directory.path().join("tls/identity.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    std::fs::write(&path, "corrupt").unwrap();
    assert!(tls::generate_self_signed(directory.path()).await.is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "corrupt");
}

#[test]
fn legacy_pins_migrate_atomically_without_changing_trust() {
    use ss_net::pins::TrustPolicy;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("pins.json");
    let id = ServerId::new("friends.example", 443).unwrap();
    let pin = fingerprint(b"legacy");
    std::fs::write(
        &path,
        serde_json::to_vec(&std::collections::BTreeMap::from([(id.as_str(), pin)])).unwrap(),
    )
    .unwrap();
    let store = JsonPinStore::open(&path).unwrap();
    assert_eq!(
        store.policy(&id).unwrap(),
        Some(TrustPolicy::PinnedCertificate { fingerprint: pin })
    );
    let data: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(data["version"], 2);
    assert_eq!(
        JsonPinStore::open(&path).unwrap().get(&id).unwrap(),
        Some(pin)
    );
    store
        .compare_exchange(
            &id,
            store.policy(&id).unwrap(),
            TrustPolicy::PublicCertificate,
        )
        .unwrap();
    assert!(store.set(&id, pin).is_err());
    assert_eq!(
        JsonPinStore::open(&path).unwrap().policy(&id).unwrap(),
        Some(TrustPolicy::PublicCertificate)
    );
    assert!(
        store
            .compare_exchange(
                &id,
                Some(TrustPolicy::PinnedCertificate { fingerprint: pin }),
                TrustPolicy::PublicCertificate
            )
            .is_err()
    );
    std::fs::write(&path, r#"{"version":99,"servers":{}}"#).unwrap();
    assert!(JsonPinStore::open(&path).is_err());
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"version":99,"servers":{}}"#
    );
}
