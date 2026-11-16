//! Invocation ids on macOS: launchd has none, so the runner makes one per
//! run from 16 bytes of `getentropy` (`sys::random_16`), in the shape
//! systemd's `INVOCATION_ID` has: 32 lowercase hex digits.

use std::fmt::Write;

/// 16 bytes as 32 lowercase hex digits.
#[must_use]
pub fn invocation_id(bytes: [u8; 16]) -> String {
    let mut s = String::with_capacity(32);
    for byte in bytes {
        write!(s, "{byte:02x}").ok();
    }
    s
}

/// Whether `text` is 32 lowercase hex digits.
#[must_use]
pub fn is_invocation_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
