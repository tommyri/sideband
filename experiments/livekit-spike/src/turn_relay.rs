//! A static-credential TURN server (webrtc-rs `turn`) on loopback. The impaired
//! LiveKit viewer is relay-only and reaches it through the impairment proxy, so
//! only that viewer's media crosses the shaped path.

use std::{net::SocketAddr, sync::Arc};

use anyhow::Result;
use serde_json::json;
use tokio::net::UdpSocket;
use turn::{
    auth::{AuthHandler, generate_auth_key},
    relay::relay_static::RelayAddressGeneratorStatic,
    server::{
        Server,
        config::{ConnConfig, ServerConfig},
    },
};
use util::vnet::net::Net;

use crate::{lk, metrics};

const REALM: &str = "spike";

struct Static(Vec<u8>);

impl AuthHandler for Static {
    fn auth_handle(
        &self,
        username: &str,
        _realm: &str,
        _src: SocketAddr,
    ) -> Result<Vec<u8>, turn::Error> {
        if username == lk::TURN_USER {
            Ok(self.0.clone())
        } else {
            Err(turn::Error::ErrFakeErr)
        }
    }
}

pub async fn run(listen: SocketAddr) -> Result<()> {
    let conn = Arc::new(UdpSocket::bind(listen).await?);
    let server = Server::new(ServerConfig {
        conn_configs: vec![ConnConfig {
            conn,
            relay_addr_generator: Box::new(RelayAddressGeneratorStatic {
                relay_address: "127.0.0.1".parse()?,
                address: "127.0.0.1".to_owned(),
                net: Arc::new(Net::new(None)),
            }),
        }],
        realm: REALM.to_owned(),
        auth_handler: Arc::new(Static(generate_auth_key(
            lk::TURN_USER,
            REALM,
            lk::TURN_PASS,
        ))),
        channel_bind_timeout: std::time::Duration::from_secs(0),
        alloc_close_notify: None,
    })
    .await?;
    metrics::event("turn", "listening", json!({"listen": listen.to_string()}));
    tokio::signal::ctrl_c().await?;
    server.close().await?;
    Ok(())
}
