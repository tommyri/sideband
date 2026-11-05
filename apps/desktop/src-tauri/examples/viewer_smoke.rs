//! Hardware-backed native-window smoke test; no camera, screen capture or server.
//! Run: cargo run -p socialspeak-desktop --example viewer_smoke
#[path = "../src/viewer_gpu.rs"]
mod viewer_gpu;
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
fn main() {
    let mut context = tauri::generate_context!();
    context.config_mut().app.windows.clear();
    tauri::Builder::default()
        .setup(|app| {
            let window = Arc::new(
                tauri::WindowBuilder::new(app, "viewer-smoke")
                    .title("Sideband native viewer smoke test")
                    .inner_size(800.0, 600.0)
                    .build()?,
            );
            let instance = wgpu::Instance::default();
            let surface = instance.create_surface(window.clone())?;
            let app = app.handle().clone();
            std::thread::spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .unwrap();
                    runtime.block_on(async {
                        let mut gpu = viewer_gpu::Gpu::new(window, instance, surface)
                            .await
                            .unwrap();
                        let mut config = moq_video::encode::Config::new(
                            320,
                            180,
                            moq_video::Rate::new(30, 1).unwrap(),
                        );
                        config.kind = moq_video::encode::Kind::Software;
                        let mut encoder = moq_video::encode::Encoder::new(&config).unwrap();
                        let mut decoder = ss_video::decode::VideoDecoder::open(
                            "avc3.42001e",
                            bytes::Bytes::new(),
                            false,
                        )
                        .unwrap();
                        let start = Instant::now();
                        let mut count = 0;
                        while start.elapsed() < Duration::from_secs(12) {
                            let mut rgba = vec![255_u8; 320 * 180 * 4];
                            for (i, p) in rgba.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                                let x = i % 320;
                                let y = i / 320;
                                p[0] = if x < 160 { 220 } else { 30 };
                                p[1] = if y < 90 { 180 } else { 30 };
                                p[2] = 80;
                            }
                            let surface =
                                moq_video::Surface::rgba(&rgba, moq_video::Size::new(320, 180))
                                    .unwrap();
                            let frame = moq_video::Frame::new(
                                surface,
                                moq_net::Timestamp::from_micros(count * 33333).unwrap(),
                            );
                            for unit in encoder.encode(&frame).unwrap() {
                                for decoded in decoder
                                    .decode(&unit.payload, (count * 1600) as u32, count == 0)
                                    .unwrap()
                                {
                                    gpu.draw(
                                        Some(&decoded),
                                        Some("Native H.264 viewer: aspect-fit / GPU / overlay"),
                                    )
                                    .unwrap();
                                }
                            }
                            count += 1;
                            tokio::time::sleep(Duration::from_millis(33)).await;
                        }
                        assert!(count > 10);
                        println!(
                            "Native viewer smoke passed: {count} frames, {}",
                            decoder.backend()
                        );
                    });
                }));
                app.exit(if result.is_ok() { 0 } else { 1 });
            });
            Ok(())
        })
        .run(context)
        .expect("viewer smoke runtime");
}
