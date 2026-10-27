//! Handshake and forwarding against the in-process spike server. No sound device.

use std::{sync::Arc, time::Duration};

use ss_core::VoiceProfile;
use ss_net::{
    pins::{MemoryPinStore, PinStore, ServerId},
    server::Endpoint,
    tls,
};
use tokio::{task::JoinHandle, time::timeout};

use crate::{
    call::{Gate, Options, join_client},
    channel::channel_from_arg,
    graph::VoiceGraph,
};

struct Fixture {
    endpoint: Endpoint,
    port: u16,
    task: JoinHandle<()>,
    pins: Arc<MemoryPinStore>,
    _data: tempfile::TempDir,
}

impl Fixture {
    async fn start() -> Self {
        let data = tempfile::tempdir().unwrap();
        let identity = tls::generate_self_signed(data.path()).await.unwrap();
        let fingerprint = tls::fingerprint(identity.certificates[0].as_ref());
        let endpoint = Endpoint::bind("127.0.0.1:0".parse().unwrap(), identity).unwrap();
        let port = endpoint.local_addr().unwrap().port();
        let pins = Arc::new(MemoryPinStore::default());
        pins.set(&ServerId::new("127.0.0.1", port).unwrap(), fingerprint)
            .unwrap();
        let server = Arc::new(ss_server::Server::default());
        let accept = endpoint.clone();
        let task = tokio::spawn(async move { server.run(accept).await });
        Self {
            endpoint,
            port,
            task,
            pins,
            _data: data,
        }
    }

    fn options(&self, name: &str, channel: ss_core::Id) -> Options {
        Options {
            host: "127.0.0.1".to_owned(),
            port: self.port,
            name: name.to_owned(),
            channel,
            profile: VoiceProfile::Voice,
            gate: Gate::Open,
            measure_latency: false,
            impairment: crate::Impairment::default(),
            bots: 0,
            record: None,
            pins: self.pins.clone(),
            duration: None,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.endpoint.close(0, b"test ended");
        self.task.abort();
    }
}

#[tokio::test]
async fn forwarded_voice_is_audible_on_the_other_client() {
    let fixture = Fixture::start().await;
    let channel = channel_from_arg("7").unwrap();
    let left = join_client(&fixture.options("left", channel), "left")
        .await
        .unwrap();
    let right = join_client(&fixture.options("right", channel), "right")
        .await
        .unwrap();
    assert!(right.remote_tracks.contains(&left.track_id));
    let mut sender = VoiceGraph::new(VoiceProfile::Voice, left.track_id).unwrap();
    let mut receiver = VoiceGraph::new(VoiceProfile::Voice, right.track_id).unwrap();
    receiver.add_track(left.track_id);
    let mut frame = vec![0.0; sender.quantum()];
    for index in 0..20_u32 {
        write_tone(&mut frame, index * 480);
        sender.push_capture(&frame, true).unwrap();
        for packet in sender.pop_outgoing() {
            left.connection.send_datagram(packet).unwrap();
        }
    }
    let mut arrival = Duration::from_millis(20);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while std::time::Instant::now() < deadline {
        match timeout(Duration::from_millis(50), right.connection.read_datagram()).await {
            Ok(Ok(bytes)) => {
                receiver.push_datagram(&bytes, arrival);
                arrival += Duration::from_millis(20);
            }
            _ => break,
        }
    }
    let mut energy = 0.0_f64;
    for step in 0..80 {
        let mixed = receiver.render(Duration::from_millis(step * 10));
        energy += mixed
            .iter()
            .map(|sample| f64::from(*sample).powi(2))
            .sum::<f64>();
    }
    assert!(energy > 0.5, "forwarded audio energy {energy}");
    assert!(receiver.snapshot().accepted > 0);
    // Keep the control streams alive until the assertions finish.
    drop(left);
    drop(right);
}

fn write_tone(frame: &mut [f32], start: u32) {
    for (index, sample) in frame.iter_mut().enumerate() {
        let time = f64::from(start.saturating_add(u32::try_from(index).unwrap_or(0))) / 48_000.0;
        #[allow(clippy::cast_possible_truncation)]
        {
            *sample = (0.25 * (2.0 * std::f64::consts::PI * 440.0 * time).sin()) as f32;
        }
    }
}
