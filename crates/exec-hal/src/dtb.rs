//! Locating the device tree blob.
//!
//! The firmware passes an address and nothing else. Turning it into a slice
//! means trusting that address, which is why this is here and not in the
//! executive: the trust is real and is recorded by the file it lives in.
//!
//! What is checked: the pointer is not null and is aligned, the magic matches,
//! and the length in the header is within a sane bound. What cannot be
//! checked: that the address points at mapped memory at all. A firmware that
//! lies about it faults, and nothing in software can prevent that.

use core::slice;

/// Largest blob accepted.
///
/// Real trees are a few kilobytes. The bound stops a corrupt length field from
/// producing a slice that covers the whole address space.
const MAX_BLOB_SIZE: usize = 1024 * 1024;

/// Smallest possible blob: the header alone.
const HEADER_SIZE: usize = 40;

/// Returns the blob at an address given by the firmware.
///
/// The lifetime is static because the firmware does not reclaim the memory.
/// The kernel must not place anything over it, which the memory map handles by
/// treating the range as reserved.
pub fn locate(address: usize) -> Option<&'static [u8]> {
    if address == 0 || address % 8 != 0 {
        return None;
    }

    // The header is read through a slice of exactly its own length, so a
    // rejected blob is never mapped in full.
    let header = unsafe { slice::from_raw_parts(address as *const u8, HEADER_SIZE) };

    let magic = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    if magic != exec_core_magic() {
        return None;
    }

    let total = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if total < HEADER_SIZE || total > MAX_BLOB_SIZE {
        return None;
    }

    // The address came from the firmware and the length from a header that
    // carries the correct magic. Nothing further can be established here.
    Some(unsafe { slice::from_raw_parts(address as *const u8, total) })
}

/// The magic value, duplicated rather than imported.
///
/// This crate sits below the executive and does not depend on it. One constant
/// is a smaller price than an edge in the dependency graph that exists for one
/// number.
const fn exec_core_magic() -> u32 {
    0xd00d_feed
}