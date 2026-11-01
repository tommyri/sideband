//! Manual listen-only smoke check. Run deliberately; start/capture may request
//! Input Monitoring. This example never posts events or changes OS permissions.
#[cfg(target_os = "macos")]
fn main() -> Result<(), ss_input::Error> {
    use ss_input::{Backend, Binding, Code, MouseButton};
    use std::{thread, time::Duration};
    let backend = Backend::detect();
    println!("Capabilities: {:?}", backend.capabilities());
    if std::env::args().nth(1).as_deref() == Some("capture") {
        println!("Hold modifiers and press a key/button within 20 seconds.");
        println!(
            "Captured: {:?}",
            backend.capture_next(Duration::from_secs(20))?
        );
        return Ok(());
    }
    let handle = backend.start(Box::new(|event| println!("{event}")))?;
    handle.update_bindings(&[Binding::key(Code::F13), Binding::mouse(MouseButton::Mouse4)])?;
    println!(
        "For 60 seconds: hold F13 or Mouse4 in the focused app. Each must still reach that app."
    );
    for _ in 0..600 {
        handle.status()?;
        thread::sleep(Duration::from_millis(100));
    }
    handle.stop()
}
#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This smoke check requires macOS.");
}
