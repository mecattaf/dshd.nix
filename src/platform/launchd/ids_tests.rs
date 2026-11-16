//! Invocation ids.

use super::*;

// catches: ids in another shape than systemd's INVOCATION_ID, which the records and `--invocation` read
#[test]
#[ignore = "packet D14: rusage conversions"]
fn ids_are_32_lowercase_hex() {
    let id = invocation_id([
        0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0, 0, 0, 0, 0, 0, 0xfe, 0xff,
    ]);
    assert_eq!(id, "0123456789abcdef000000000000feff");
    assert!(is_invocation_id(&id));
    assert!(!is_invocation_id("0123456789ABCDEF000000000000FEFF"));
    assert!(!is_invocation_id("0123"));
    assert!(!is_invocation_id("0123456789abcdef000000000000feffaa"));
    assert!(!is_invocation_id("0123456789abcdef000000000000fefg"));
}
