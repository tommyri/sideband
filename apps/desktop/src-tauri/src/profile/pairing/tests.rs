use super::*;
use crate::profile::{self, Vault, tests::MemoryVault};
use zeroize::Zeroizing;

#[test]
fn only_the_reviewed_transcript_and_requester_can_open_the_transfer() {
    let owner = Invitation::new(false).unwrap();
    let guest = Invitation::parse(&owner.code().to_ascii_lowercase()).unwrap();
    let slot = owner.slot("relay|443|public");
    assert_ne!(slot, owner.slot("relay|8443|public"));
    let a = Exchange::new().unwrap();
    let b = Exchange::new().unwrap();
    let offer = owner.offer(&slot, a.public);
    let claim = guest.claim(&slot, b.public, &[1; 32], "Windows PC".into());
    let replaced = Exchange::new().unwrap();
    let other = guest.claim(&slot, replaced.public, &[2; 32], "Windows PC".into());
    for altered in [
        Claim {
            name: "Different PC".into(),
            ..claim.clone()
        },
        Claim {
            reader: [0; 32],
            ..claim.clone()
        },
        Claim {
            key: other.key,
            ..claim.clone()
        },
    ] {
        assert!(owner.verify(&slot, &offer, &altered).is_err());
    }
    assert!(
        guest
            .verify(
                &slot,
                &Offer {
                    key: other.key,
                    ..offer.clone()
                },
                &claim
            )
            .is_err()
    );
    let send = a.finish(&owner, &slot, &offer, &claim, claim.key).unwrap();
    let receive = b.finish(&guest, &slot, &offer, &claim, offer.key).unwrap();
    let wrong = replaced
        .finish(&guest, &slot, &offer, &other, offer.key)
        .unwrap();
    assert_eq!(send.phrase, receive.phrase);
    let source = tempfile::tempdir().unwrap();
    let dest = tempfile::tempdir().unwrap();
    let old = source.path().join("profile.json");
    let new = dest.path().join("profile.json");
    let vault = MemoryVault::default();
    profile::create(&old, "Sam", &vault).unwrap();
    let bytes = backup::transfer(&old, &vault).unwrap();
    let encrypted = send.seal(bytes).unwrap();
    assert!(wrong.open(&encrypted).is_err());
    assert!(receive.open(&(encrypted.clone() + "x")).is_err());
    let mut clear = receive.open(&encrypted).unwrap();
    assert_eq!(
        backup::install_transfer(&new, &clear, &vault).unwrap(),
        "Sam"
    );
    assert!(backup::install_transfer(&new, &clear, &vault).is_err());
    clear.fill(0);
    let secrets = |p| vault.read(&record(p).unwrap().unwrap().key_id).unwrap();
    let (first, second) = (secrets(&old), secrets(&new));
    assert_eq!(&first[..32], &second[..32]);
    assert_ne!(&first[32..], &second[32..]);
    assert!(
        send.seal(Zeroizing::new(vec![0; ss_core::pairing::MAX_DELIVERY]))
            .is_err()
    );
}

#[cfg(any(target_os = "macos", target_os = "windows"))]
mod desktop {
    use super::*;
    use ss_client_core::storage::Storage;
    use ss_net::pins::PinStore;
    use tauri::{
        App, Manager,
        test::{MockRuntime, mock_app},
    };

    // Remove both independently generated credential-store items, also on unwind.
    struct Computer {
        app: App<MockRuntime>,
        dir: tempfile::TempDir,
    }
    impl Computer {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let app = mock_app();
            app.manage(AppState::open(dir.path()).unwrap());
            app.manage(Pairings::default());
            Self { app, dir }
        }
        fn state(&self) -> State<'_, AppState> {
            self.app.state()
        }
        async fn action(&self, action: PairAction) -> Result<PairView, ApiError> {
            pair_update(self.state(), self.app.state(), "dialog".into(), action).await
        }
        async fn join(&self, target: &ProbeInput, code: String) -> Result<PairView, ApiError> {
            pair_join(
                self.state(),
                self.app.state(),
                "dialog".into(),
                target.clone(),
                code,
                "Windows PC".into(),
            )
            .await
        }
        async fn invite(&self) -> String {
            let PairView::Invite { code, .. } =
                pair_start(self.state(), self.app.state(), "dialog".into())
                    .await
                    .unwrap()
            else {
                panic!("expected invitation")
            };
            code
        }
        async fn connect(&self, addr: &ServerAddr, invite: String) -> Client {
            let state = self.state();
            let id = addr.id().unwrap();
            catalog::seed_trust(&state.profile_path, &*state.pins, &id).unwrap();
            let creds = profile::credentials(state.profile_path.clone(), id, "sam".into(), invite)
                .await
                .unwrap();
            Client::connect_with_storage(
                addr.clone(),
                creds,
                state.pins.clone(),
                state.storage.clone(),
            )
            .await
            .unwrap()
        }
    }
    impl Drop for Computer {
        fn drop(&mut self) {
            if let Ok(Some(rec)) = record(&self.dir.path().join("profile.json")) {
                let _ = vault::System.remove(&rec.key_id);
            }
        }
    }
    struct World {
        _dir: tempfile::TempDir,
        endpoint: ss_net::server::Endpoint,
        quic: tokio::task::JoinHandle<()>,
        https: tokio::task::JoinHandle<()>,
        target: ProbeInput,
        address: ServerAddr,
        invite: String,
    }
    impl World {
        async fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let pool = Arc::new(
                ss_server::db::Pool::open(dir.path().join("relay.db"))
                    .await
                    .unwrap(),
            );
            let invite = ss_server::auth::ensure_owner_invite(&pool)
                .await
                .unwrap()
                .unwrap();
            let identity =
                ss_server::tls::prepare(&ss_server::config::Config::default(), dir.path())
                    .await
                    .unwrap();
            let endpoint = ss_net::server::Endpoint::bind_resolver(
                "127.0.0.1:0".parse().unwrap(),
                identity.resolver(),
            )
            .unwrap();
            let server = Arc::new(
                ss_server::connection::Server::open(pool.clone())
                    .await
                    .unwrap(),
            );
            let quic = tokio::spawn(server.clone().run(endpoint.clone()));
            let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let target = ProbeInput {
                host: "127.0.0.1".into(),
                port: endpoint.local_addr().unwrap().port(),
                https_port: tcp.local_addr().unwrap().port(),
            };
            let mut address = ServerAddr::new(&target.host, target.port).unwrap();
            address.https_port = target.https_port;
            let https = tokio::spawn(ss_server::http::hardening::serve_tls(
                tcp,
                ss_server::http::info::router(pool, identity.fingerprint_slot())
                    .merge(server.public_routes()),
                identity.https_config(),
                std::future::pending(),
            ));
            Self {
                _dir: dir,
                endpoint,
                quic,
                https,
                target,
                address,
                invite,
            }
        }
        async fn enroll(&self, mac: &Computer) {
            let state = mac.state();
            profile::create_local_profile(mac.state(), "Sam".into())
                .await
                .unwrap();
            let probe = onboarding::probe(self.target.clone(), state.pins.clone())
                .await
                .unwrap();
            let id = self.address.id().unwrap();
            state
                .pins
                .set(&id, Fingerprint::try_from(probe.fingerprint).unwrap())
                .unwrap();
            let client = mac.connect(&self.address, self.invite.clone()).await;
            let mut settings = client.server_settings().await.unwrap().settings;
            settings.profile_sync = Some(true);
            client.save_server_settings(&settings).await.unwrap();
            catalog::remember_client(&state, &client).await.unwrap();
            state.clients().insert(client).unwrap();
            relay::choose_sync_server(mac.state(), id.as_str().into())
                .await
                .unwrap();
        }
    }
    impl Drop for World {
        fn drop(&mut self) {
            self.endpoint.close(0, b"done");
            self.https.abort();
            self.quic.abort();
        }
    }
    #[tokio::test]
    #[ignore = "uses temporary entries in the real OS credential store and a loopback relay"]
    async fn two_desktop_installations_pair_and_enroll_as_the_same_server_member() {
        let world = World::new().await;
        let mac = Computer::new();
        let pc = Computer::new();
        world.enroll(&mac).await;
        let stale = mac.invite().await;
        let denied = mac.invite().await;
        assert!(pc.join(&world.target, stale).await.is_err());
        pc.join(&world.target, denied.clone()).await.unwrap();
        assert!(matches!(
            mac.action(PairAction::Check).await.unwrap(),
            PairView::Review { .. }
        ));
        mac.action(PairAction::Cancel).await.unwrap();
        assert!(pc.action(PairAction::Check).await.is_err());
        assert!(pc.join(&world.target, denied).await.is_err());
        assert!(!pc.state().profile_path.exists());
        pc.join(&world.target, mac.invite().await).await.unwrap();
        pc.action(PairAction::Cancel).await.unwrap();
        assert!(mac.action(PairAction::Check).await.is_err());
        let code = mac.invite().await;
        let PairView::Compare { phrase, .. } = pc.join(&world.target, code.clone()).await.unwrap()
        else {
            panic!("missing phrase")
        };
        assert!(matches!(
            pc.action(PairAction::Check).await.unwrap(),
            PairView::Waiting
        ));
        assert!(mac.action(PairAction::Approve).await.is_err());
        let PairView::Review {
            phrase: other,
            device,
            memberships,
        } = mac.action(PairAction::Check).await.unwrap()
        else {
            panic!("missing review")
        };
        assert_eq!(
            (phrase, device.as_str(), memberships),
            (other, "Windows PC", 1)
        );
        assert!(!pc.state().profile_path.exists());
        mac.action(PairAction::Approve).await.unwrap();
        assert!(matches!(
            pc.action(PairAction::Check).await.unwrap(),
            PairView::Ready
        ));
        assert!(
            matches!(pc.action(PairAction::Install).await.unwrap(),PairView::Linked{name} if name=="Sam")
        );
        assert!(Computer::new().join(&world.target, code).await.is_err());
        let id = world.address.id().unwrap();
        assert!(pc.state().storage.session(&id).unwrap().is_none());
        let new = pc.connect(&world.address, String::new()).await;
        let old = mac.state().client(&id).unwrap();
        let member = |c: &Client| c.state().read().own_user.clone().unwrap();
        assert_eq!(member(&new).id, member(&old).id);
        new.server_settings().await.unwrap(); // Both sessions retain manager access.
        assert_ne!(
            pc.state().storage.session(&id).unwrap(),
            mac.state().storage.session(&id).unwrap()
        );
        new.close().await;
    }
}
