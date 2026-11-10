//! The web announcement: recognising `dsh web: URL` on the child's output,
//! taking the port from it, and redacting the token for anything that is
//! not the 0600 `web-url` file. Readiness is reported only once the URL is
//! recorded where `attach` finds it; a start whose URL cannot be recorded
//! fails instead.

use std::io::{self, BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::error::Error;
use crate::name::{InstanceDir, RuntimeDir};
use crate::runner::notify::Notify;
use crate::runner::{log_line, mark_ready, signal};
use crate::{fsx, instance, out};

/// The prefix DSH prints before the authenticated URL.
const ANNOUNCEMENT_PREFIX: &str = "dsh web: ";

/// What stands in for a token value outside the 0600 `web-url` file.
const REDACTED: &str = "REDACTED";
/// The token query key, compared without regard to ASCII case.
const TOKEN_KEY: &[u8] = b"token=";
/// The same key percent-encoded, as it appears in a URL nested in another.
const TOKEN_KEY_ENCODED: &[u8] = b"token%3d";
/// Longest line read from `dsh`'s stdout, the relay's frame bound; the rest
/// of a longer line is discarded.
const LINE_CAP: u64 = 64 * 1024 * 1024;
/// Mode of the `web-url` file: it carries the token.
const WEB_URL_MODE: u32 = 0o600;

/// Returns the URL when `line` is the announcement.
#[must_use]
fn parse_announcement(line: &str) -> Option<String> {
    let url = line.strip_prefix(ANNOUNCEMENT_PREFIX)?.trim();
    (url.starts_with("http://") || url.starts_with("https://")).then(|| url.to_owned())
}

/// The TCP port of an announced URL.
#[must_use]
pub fn port_of(url: &str) -> Option<u16> {
    let (_scheme, rest) = url.split_once("://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_userinfo, host_port)| host_port);
    let port = match host_port.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?.1.strip_prefix(':')?,
        None => host_port.rsplit_once(':')?.1,
    };
    port.parse().ok()
}

/// Replaces the value after `token=` or `token%3D` with `REDACTED`.
///
/// The key is matched without regard to ASCII case. A value ends at `&`,
/// `#`, whitespace, a quote or an angle bracket (none of which a URL holds
/// unencoded), and in the encoded form also at an encoded `&` or `#`.
#[must_use]
pub fn redact_tokens(url: &str) -> String {
    let mut redacted = String::with_capacity(url.len());
    let mut rest = url;
    while let Some((key_end, encoded)) = find_key(rest.as_bytes()) {
        // Every split point is at an ASCII byte, hence a char boundary.
        let (head, tail) = rest.split_at(key_end);
        redacted.push_str(head);
        redacted.push_str(REDACTED);
        rest = tail.split_at(value_len(tail.as_bytes(), encoded)).1;
    }
    redacted.push_str(rest);
    redacted
}

/// The stdout pump of a web instance: forwards every line to the journal
/// with tokens redacted, and acts on the first announcement of the run.
#[derive(Debug)]
pub(crate) struct Pump {
    /// Where `web-url` goes.
    pub(crate) runtime: RuntimeDir,
    /// Where `web-port` goes.
    pub(crate) dir: InstanceDir,
    /// No port is pinned yet: pin the announced one.
    pub(crate) pin_port: bool,
    /// The service manager, when the runner runs under one.
    pub(crate) notify: Option<Arc<Notify>>,
    /// Set once readiness was reported.
    pub(crate) ready: Arc<AtomicBool>,
    /// Set, with the main thread woken, when the announcement cannot be
    /// recorded: the start failed and the main thread stops dsh.
    pub(crate) stop_asked: Arc<AtomicBool>,
}

impl Pump {
    /// Reads `stdout` until `dsh` closes it. Lines are read lossily and
    /// capped, so no byte `dsh` prints can make the runner drop the pipe.
    pub(crate) fn run(&self, stdout: impl Read) {
        if let Err(error) = self.forward(&mut BufReader::new(stdout)) {
            log_line(&format!("stopped reading dsh's output: {error}"));
        }
    }

    fn forward(&self, reader: &mut impl BufRead) -> io::Result<()> {
        let mut line = Vec::new();
        let mut announced = false;
        while read_line(reader, &mut line, LINE_CAP)? {
            let text = String::from_utf8_lossy(&line); // gate: allow R5 (journal text, the token redacted below)
            let text = text.trim_end_matches(['\n', '\r']);
            if !announced && let Some(url) = parse_announcement(text) {
                announced = true;
                if let Err(error) = self.announce(&url) {
                    log_line(&format!(
                        "{error}; the start cannot be attached, so dsh is stopped"
                    ));
                    signal::ask_stop(&self.stop_asked);
                }
            }
            out::error_line(&redact_tokens(text));
        }
        Ok(())
    }

    /// The first announcement: `web-url` (0600, token included), the port
    /// pin when none exists yet, then `READY=1` and a redacted `STATUS=`.
    /// A write that fails reports no readiness: without `web-url` nothing
    /// can attach, and without the pin a restart would move the origin the
    /// browser's cookies are bound to.
    fn announce(&self, url: &str) -> Result<(), Error> {
        fsx::write_atomic(&self.runtime.web_url(), url.as_bytes(), WEB_URL_MODE)?;
        if self.pin_port {
            match port_of(url) {
                Some(port) => instance::write_web_port(&self.dir, port)?,
                None => log_line("the announced URL names no port, so none is pinned"),
            }
        }
        let shown = redact_tokens(url);
        let status = format!("web {shown}");
        mark_ready(self.notify.as_deref(), &self.ready, Some(status.as_str()));
        Ok(())
    }
}

/// The end of the first token key in `text`, and whether it is the
/// percent-encoded form.
fn find_key(text: &[u8]) -> Option<(usize, bool)> {
    (0..text.len()).find_map(|at| {
        let tail = text.get(at..)?;
        if starts_with_ignore_case(tail, TOKEN_KEY) {
            Some((at + TOKEN_KEY.len(), false))
        } else if starts_with_ignore_case(tail, TOKEN_KEY_ENCODED) {
            Some((at + TOKEN_KEY_ENCODED.len(), true))
        } else {
            None
        }
    })
}

/// Length of the token value at the start of `text`.
fn value_len(text: &[u8], encoded: bool) -> usize {
    let ends_value = |at: usize| match text.get(at..) {
        Some([first, ..]) if first.is_ascii_whitespace() || b"&#\"'<>".contains(first) => true,
        Some(tail) => {
            encoded
                && (starts_with_ignore_case(tail, b"%26") || starts_with_ignore_case(tail, b"%23"))
        }
        None => true,
    };
    (0..text.len())
        .find(|&at| ends_value(at))
        .unwrap_or(text.len())
}

fn starts_with_ignore_case(text: &[u8], prefix: &[u8]) -> bool {
    text.get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Reads one line of at most `cap` bytes into `line`, newline included when
/// there was one, and skips the rest of a longer line. `false` at EOF.
fn read_line(reader: &mut impl BufRead, line: &mut Vec<u8>, cap: u64) -> io::Result<bool> {
    line.clear();
    if Read::take(&mut *reader, cap).read_until(b'\n', line)? == 0 {
        return Ok(false);
    }
    if !line.ends_with(b"\n") {
        reader.skip_until(b'\n')?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    // catches: readiness taken from a line that is not DSH's announcement, or an announcement missed (P3.1)
    #[test]
    fn parse_announcement() {
        assert_eq!(
            super::parse_announcement("dsh web: http://127.0.0.1:3080/?token=abc  \r"),
            Some("http://127.0.0.1:3080/?token=abc".to_owned())
        );
        assert_eq!(
            super::parse_announcement("dsh web: https://[::1]:8443/"),
            Some("https://[::1]:8443/".to_owned())
        );
        assert_eq!(super::parse_announcement("dsh web: starting"), None);
        assert_eq!(
            super::parse_announcement("  dsh web: http://127.0.0.1:1/"),
            None
        );
        assert_eq!(
            super::parse_announcement("listening on http://127.0.0.1:1/"),
            None
        );
    }

    // catches: a token that reaches the journal or the status line in any case or encoding
    #[test]
    fn redact_tokens_both_encodings() {
        let cases = [
            (
                "http://127.0.0.1:3080/?token=abc123",
                "http://127.0.0.1:3080/?token=REDACTED",
            ),
            (
                "http://h:1/?a=1&token=s3cr3t&b=2#top",
                "http://h:1/?a=1&token=REDACTED&b=2#top",
            ),
            (
                "http://h:1/login?next=%2F%3Ftoken%3Dabc%26x%3D1",
                "http://h:1/login?next=%2F%3Ftoken%3DREDACTED%26x%3D1",
            ),
            (
                "next=%2f%3ftoken%3dabc%23frag",
                "next=%2f%3ftoken%3dREDACTED%23frag",
            ),
            (
                "dsh web: http://h:1/?TOKEN=abc (ctrl-c stops)",
                "dsh web: http://h:1/?TOKEN=REDACTED (ctrl-c stops)",
            ),
            (
                "\"url\":\"http://h:1/?token=a+b/c=\"",
                "\"url\":\"http://h:1/?token=REDACTED\"",
            ),
            ("token=one token=two", "token=REDACTED token=REDACTED"),
            ("token=", "token=REDACTED"),
            ("café token=ü€", "café token=REDACTED"),
            ("no secret here", "no secret here"),
        ];
        for (input, expected) in cases {
            assert_eq!(redact_tokens(input), expected, "{input}");
        }
    }

    // catches: a wrong port pinned from the announcement, so a restart moves the browser's origin (P3.1)
    #[test]
    fn port_of_url() {
        assert_eq!(port_of("http://127.0.0.1:3080/?token=abc"), Some(3080));
        assert_eq!(port_of("https://user:pw@example.test:8443"), Some(8443));
        assert_eq!(port_of("http://[::1]:9000/path"), Some(9000));
        assert_eq!(port_of("http://localhost:1#frag"), Some(1));
        assert_eq!(port_of("http://localhost/"), None);
        assert_eq!(port_of("http://[::1]/"), None);
        assert_eq!(port_of("http://localhost:99999/"), None);
        assert_eq!(port_of("localhost:80"), None);
    }

    // catches: an endless line from dsh growing the runner without bound, or the lines after it lost
    #[test]
    fn long_lines_are_capped_and_resynchronised() {
        let mut reader = io::Cursor::new(b"ab\ncdefgh\nij".to_vec());
        let mut line = Vec::new();
        let mut lines = Vec::new();
        while read_line(&mut reader, &mut line, 4).unwrap() {
            lines.push(String::from_utf8(line.clone()).unwrap());
        }
        assert_eq!(lines, ["ab\n", "cdef", "ij"]);
    }
}
