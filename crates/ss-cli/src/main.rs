//! Headless SocialSpeak client.
//!
//! Used for the voice spike, latency measurement, and load tests. The
//! desktop app does not link this binary.

fn main() {
    println!("{}", ss_client_core::NAME);
}

#[cfg(test)]
mod tests {
    #[test]
    fn links_client_core() {
        assert_eq!(ss_client_core::NAME, "ss-client-core");
    }
}
