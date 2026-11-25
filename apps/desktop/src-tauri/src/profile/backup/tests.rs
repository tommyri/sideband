use super::*;
use crate::profile::{catalog::Membership, tests::MemoryVault};
use ss_net::pins::TrustPolicy;

const WORDS: &str =
    "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

fn sample() -> Body {
    let member = Membership {
        host: "friends.example".into(),
        port: 443,
        https_port: 8443,
        name: "Friday Games".into(),
        username: "sam".into(),
        user_id: Id::from_bytes([1; 16]).to_string(),
        trust: TrustPolicy::PinnedCertificate {
            fingerprint: "ab".repeat(32).try_into().unwrap(),
        },
    };
    let server = member.server_id().unwrap();
    let proof = ProfileCredentials::new(&[7; 32], &[9; 32], &server, String::new(), String::new())
        .unwrap()
        .recovery_grant(member.user_id.parse().unwrap());
    Body {
        profile: PortableProfile {
            name: "Sam".into(),
            memberships: vec![member],
        },
        grants: BTreeMap::from([(
            server.as_str().to_owned(),
            Grant {
                source: proof.source.to_vec(),
                authorization: proof.authorization.to_vec(),
            },
        )]),
    }
}

#[test]
fn v1_backup_interchanges_across_platforms_and_restores_offline() {
    let body = sample();
    let (bytes, words) = format::seal_with(&[7; 32], &body, &[0; 16], [4; 12]).unwrap();
    assert_eq!(words, WORDS);
    // Exported by the production encoder on macOS. Every desktop target
    // reads these exact bytes before unlocking and reinstalling the profile.
    let reference = include_bytes!("fixtures/v1.sideband-backup");
    assert_eq!(bytes.as_slice(), reference);
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("Test.sideband-backup");
    save_new(&file, reference).unwrap();
    assert_eq!(
        save_new(&file, b"replacement").unwrap_err().code,
        "backup_exists"
    );
    let bytes = fs::read(file).unwrap();
    let new = dir.path().join("new/profile.json");
    let vault = MemoryVault::default();
    assert_eq!(restore(&new, &bytes, WORDS, &vault).unwrap(), "Sam");
    assert_eq!(catalog::snapshot(&new).unwrap(), body.profile);
    let record = super::record(&new).unwrap().unwrap();
    let roots = vault.read(&record.key_id).unwrap();
    assert_eq!(&roots[..32], &[7; 32]);
    assert_ne!(&roots[32..], &[9; 32]);
    let serialized = fs::read(&new).unwrap();
    assert!(
        String::from_utf8(serialized)
            .unwrap()
            .contains("Friday Games")
    );
    // Before this installation ever reaches the server it can export again.
    let (again, next_words) = export(&new, &vault).unwrap();
    let (root, next) = format::open(&again, &next_words).unwrap();
    assert_eq!(root.as_ref(), &[7; 32]);
    assert_eq!(next.profile, body.profile);
    assert_eq!(
        next.grants.values().next().unwrap().source,
        body.grants.values().next().unwrap().source
    );
    assert_ne!(next_words, words);
}

#[test]
fn rejected_restores_preserve_local_state_and_report_the_remedy() {
    let body = sample();
    let (bytes, words) = format::seal(&[7; 32], &body).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let profile = dir.path().join("profile.json");
    let vault = MemoryVault::default();
    assert_eq!(
        restore(&profile, &bytes, WORDS, &vault).unwrap_err().code,
        "backup_key"
    );
    let mut broken = bytes.clone();
    broken[33] ^= 0x40;
    assert_eq!(
        restore(&profile, &broken, &words, &vault).unwrap_err().code,
        "backup_damaged"
    );
    // Even a recomputed unkeyed transport checksum cannot authenticate changed ciphertext.
    let end = broken.len() - 32;
    let sum = ring::digest::digest(&ring::digest::SHA256, &broken[..end]);
    broken[end..].copy_from_slice(sum.as_ref());
    assert_eq!(
        restore(&profile, &broken, &words, &vault).unwrap_err().code,
        "backup_key"
    );
    assert!(!profile.exists());
    crate::profile::create(&profile, "Existing", &vault).unwrap();
    let original = fs::read(&profile).unwrap();
    assert_eq!(
        restore(&profile, &bytes, &words, &vault).unwrap_err().code,
        "profile_exists"
    );
    assert_eq!(fs::read(profile).unwrap(), original);
}
