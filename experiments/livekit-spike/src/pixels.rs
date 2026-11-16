//! Luma-plane access to a CoreVideo pixel buffer, for stamping a captured
//! frame in place (no copy) and for reading a decoded one.

use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane,
    CVPixelBufferGetHeightOfPlane, CVPixelBufferGetWidthOfPlane, CVPixelBufferIsPlanar,
    CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

/// Run `f(luma, stride, width)` on plane 0 of a planar YUV buffer.
pub fn with_luma<R>(
    buffer: &CVPixelBuffer,
    write: bool,
    f: impl FnOnce(&mut [u8], usize, usize) -> R,
) -> Option<R> {
    if !CVPixelBufferIsPlanar(buffer) {
        return None;
    }
    let flags = if write {
        CVPixelBufferLockFlags(0)
    } else {
        CVPixelBufferLockFlags::ReadOnly
    };
    // SAFETY: the buffer is a live CVPixelBuffer; the lock pins its base address
    // until the matching unlock below.
    if unsafe { CVPixelBufferLockBaseAddress(buffer, flags) } != 0 {
        return None;
    }
    let base = CVPixelBufferGetBaseAddressOfPlane(buffer, 0);
    let stride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0);
    let width = CVPixelBufferGetWidthOfPlane(buffer, 0);
    let height = CVPixelBufferGetHeightOfPlane(buffer, 0);
    let result = if base.is_null() {
        None
    } else {
        // Only the barcode rows are touched; the slice spans the whole plane.
        // SAFETY: plane 0 is `stride * height` bytes while locked.
        let plane = unsafe { std::slice::from_raw_parts_mut(base.cast::<u8>(), stride * height) };
        Some(f(plane, stride, width))
    };
    // SAFETY: matches the lock above.
    unsafe { CVPixelBufferUnlockBaseAddress(buffer, flags) };
    result
}
