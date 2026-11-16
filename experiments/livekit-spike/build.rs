// libwebrtc's Objective-C categories (VideoToolbox codec factory, NSString
// helpers) live in a static library; without -ObjC the linker drops them and
// the peer-connection factory throws on the first H.264 query.
fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var_os("CARGO_FEATURE_LIVEKIT").is_some()
    {
        println!("cargo:rustc-link-arg=-ObjC");
    }
}
