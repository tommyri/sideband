//! SocialSpeak server.
//!
//! One process: QUIC for control and voice, HTTPS for uploads and history,
//! SQLite for persistence.

fn main() {
    let layers = ss_net::foundation();
    println!("{} ({}, {})", env!("CARGO_PKG_NAME"), layers[0], layers[1]);
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_the_protocol_stack() {
        assert_eq!(ss_net::foundation(), ["ss-core", "ss-proto"]);
        assert_eq!(ss_core::NAME, "ss-core");
        assert_eq!(ss_proto::NAME, "ss-proto");
    }
}
