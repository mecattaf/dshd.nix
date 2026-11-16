//! Invocation ids on macOS: launchd has none, so the runner makes one per
//! run from 16 bytes of `getentropy` (`sys::random_16`), in the shape
//! systemd's `INVOCATION_ID` has: 32 lowercase hex digits.

/// 16 bytes as 32 lowercase hex digits.
#[must_use]
pub fn invocation_id(bytes: [u8; 16]) -> String {
    let _ = bytes;
    String::new()
}

/// Whether `text` is 32 lowercase hex digits.
#[must_use]
pub fn is_invocation_id(text: &str) -> bool {
    let _ = text;
    false
}

#[cfg(test)]
#[path = "ids_tests.rs"]
mod tests;
