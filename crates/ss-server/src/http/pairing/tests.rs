use super::*;
use ss_core::Id;

#[test]
fn only_the_originating_session_and_immutable_claim_can_finish_before_expiry() {
    let boxes = Mailboxes::default();
    let member = Member {
        user: Id::from_bytes([1; 16]),
        session: Id::from_bytes([2; 16]),
        membership_key: [3; 32],
    };
    let slot = [4; 32];
    let owner = |action| boxes.owner_action(member.clone(), Request { slot, action });
    let device = |action| boxes.guest_action(&member, Request { slot, action });
    let offer = Offer {
        key: [5; 32],
        proof: [6; 32],
    };
    let token = [7; 32];
    let claim = Claim {
        key: [8; 32],
        name: "Windows PC".into(),
        reader: Sha256::digest(token).into(),
        proof: [9; 32],
    };
    let approve = || Owner::Approve {
        proof: claim.proof,
        delivery: "sealed".into(),
    };
    owner(Owner::Start(offer.clone())).unwrap();
    assert!(owner(approve()).is_err());
    device(Guest::Join(claim.clone())).unwrap();
    assert!(matches!(
        device(Guest::Join(claim.clone())),
        Ok(Reply::Offered(_))
    ));
    assert!(matches!(
        device(Guest::Join(Claim {
            key: [10; 32],
            ..claim.clone()
        })),
        Err(StatusCode::CONFLICT)
    ));
    assert!(matches!(device(Guest::Poll(token)), Ok(Reply::Waiting)));
    assert!(
        boxes
            .owner_action(
                Member {
                    session: Id::from_bytes([11; 16]),
                    ..member.clone()
                },
                Request {
                    slot,
                    action: approve()
                }
            )
            .is_err()
    );
    assert!(
        owner(Owner::Approve {
            proof: [0; 32],
            delivery: "sealed".into()
        })
        .is_err()
    );
    owner(approve()).unwrap();
    owner(approve()).unwrap();
    assert!(device(Guest::Poll([0; 32])).is_err());
    assert!(device(Guest::Finish([0; 32])).is_err());
    for _ in 0..2 {
        assert!(matches!(device(Guest::Poll(token)),Ok(Reply::Delivery(v)) if v=="sealed"));
    }
    device(Guest::Finish(token)).unwrap();
    assert!(device(Guest::Join(claim.clone())).is_err());
    owner(Owner::Start(offer)).unwrap();
    device(Guest::Join(claim.clone())).unwrap();
    owner(approve()).unwrap();
    boxes.0.lock().unwrap().get_mut(&slot).unwrap().until = Instant::now();
    assert!(matches!(device(Guest::Poll(token)), Err(GONE)));
    assert!(matches!(owner(Owner::Inspect), Err(GONE)));
}
