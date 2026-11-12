//! A run's journal as dsh wrote it. journald stores a stream line longer
//! than its `LineMax` (48K by default) as several entries and marks every
//! one but the last `_LINE_BREAK=line-max`; journalctl's JSON form keeps
//! that mark, so the pieces are joined back into the one line. `read`,
//! `run`, `wait` and `read --follow` read entries through this module.

use serde_json::Value;

/// journalctl's output arguments for entries [`Entry::parse`] reads: the
/// two fields it needs, in full (without `--all` JSON nulls a field over
/// 4096 bytes).
pub(super) const OUTPUT: [&str; 4] = ["-o", "json", "--all", "--output-fields=MESSAGE,_LINE_BREAK"];

/// One journal entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Entry {
    /// None for an entry without a `MESSAGE`, which the text form skips.
    message: Option<String>,
    /// journald broke the line here, at `LineMax`: the next entry goes on
    /// with it.
    broken: bool,
}

impl Entry {
    /// One line of journalctl's JSON output. `MESSAGE` is a string, or an
    /// array of bytes when it is not printable UTF-8; those bytes are
    /// replaced as the text form shows them. A line that is not an entry is
    /// kept as it is, so nothing journalctl printed is lost.
    pub(super) fn parse(line: &[u8]) -> Self {
        let Ok(Value::Object(fields)) = serde_json::from_slice::<Value>(line) else {
            return Self {
                message: Some(String::from_utf8_lossy(line).into_owned()),
                broken: false,
            };
        };
        let message = match fields.get("MESSAGE") {
            None => None,
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Array(bytes)) => {
                let bytes: Vec<u8> = bytes
                    .iter()
                    .filter_map(|byte| byte.as_u64().and_then(|byte| u8::try_from(byte).ok()))
                    .collect();
                Some(String::from_utf8_lossy(&bytes).into_owned())
            }
            Some(_) => Some(String::new()),
        };
        let broken = fields.get("_LINE_BREAK").and_then(Value::as_str) == Some("line-max");
        Self { message, broken }
    }

    /// journald broke the line at this entry.
    pub(super) fn broken(&self) -> bool {
        self.broken
    }
}

/// Joins entries, in order, into the lines dsh wrote.
#[derive(Debug, Default)]
pub(super) struct Joiner {
    /// A broken line whose last piece has not come yet.
    open: Option<String>,
}

impl Joiner {
    /// Takes the next entry; the lines it ends, if it ends one. A message
    /// that holds newlines is as many lines as the text form prints.
    pub(super) fn push(&mut self, entry: Entry) -> Vec<String> {
        let Some(piece) = entry.message else {
            return Vec::new();
        };
        let message = match self.open.take() {
            Some(mut start) => {
                start.push_str(&piece);
                start
            }
            None => piece,
        };
        if entry.broken {
            self.open = Some(message);
            return Vec::new();
        }
        split(&message)
    }

    /// The lines of a broken line whose last piece never came, as far as
    /// it goes.
    pub(super) fn finish(self) -> Vec<String> {
        self.open.as_deref().map(split).unwrap_or_default()
    }
}

/// The lines of `entries`; a line still open at the end is kept as far as
/// it goes.
pub(super) fn lines(entries: impl IntoIterator<Item = Entry>) -> Vec<String> {
    let mut joiner = Joiner::default();
    let mut lines: Vec<String> = entries
        .into_iter()
        .flat_map(|entry| joiner.push(entry))
        .collect();
    lines.extend(joiner.finish());
    lines
}

/// `message` as the lines `-o cat` prints for it.
fn split(message: &str) -> Vec<String> {
    message
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(json: &str) -> Entry {
        Entry::parse(json.as_bytes())
    }

    // catches: the pieces of a line journald broke at LineMax returned as
    // separate lines, or a line after them joined to them
    #[test]
    fn broken_pieces_join_into_one_line() {
        let entries = [
            r#"{"MESSAGE":"dshd-run: run started"}"#,
            r#"{"MESSAGE":"{\"type\":\"final\",\"text\":\"a ","_LINE_BREAK":"line-max"}"#,
            r#"{"MESSAGE":"long ","_LINE_BREAK":"line-max"}"#,
            r#"{"MESSAGE":"answer\"}","__CURSOR":"s=1"}"#,
            r#"{"MESSAGE":"after"}"#,
        ];
        assert_eq!(
            lines(entries.map(entry)),
            [
                "dshd-run: run started",
                r#"{"type":"final","text":"a long answer"}"#,
                "after"
            ]
        );
    }

    // catches: a line journald broke for another reason (eof, nul,
    // pid-change) joined to the next line
    #[test]
    fn only_line_max_continues() {
        let entries = [
            r#"{"MESSAGE":"cut at exit","_LINE_BREAK":"eof"}"#,
            r#"{"MESSAGE":"next"}"#,
        ];
        assert_eq!(lines(entries.map(entry)), ["cut at exit", "next"]);
    }

    // catches: a broken line still being written dropped
    #[test]
    fn an_open_line_is_kept_as_far_as_it_goes() {
        let mut joiner = Joiner::default();
        assert!(
            joiner
                .push(entry(r#"{"MESSAGE":"so far","_LINE_BREAK":"line-max"}"#))
                .is_empty()
        );
        assert_eq!(joiner.finish(), ["so far"]);
    }

    // catches: a message that is not printable UTF-8 (an array of bytes in
    // JSON) lost, a newline inside one message not split as the text form
    // does, an entry without a message shown as a line the text form does
    // not print, or a line that is not JSON dropped
    #[test]
    fn bytes_newlines_and_other_lines_read_as_the_text_form() {
        let colored = entry(r#"{"MESSAGE":[27,91,49,109,104,105,255]}"#);
        assert_eq!(lines([colored]), ["\u{1b}[1mhi\u{fffd}"]);
        assert_eq!(
            lines([entry(r#"{"MESSAGE":"one\r\ntwo\n"}"#)]),
            ["one", "two", ""]
        );
        assert_eq!(lines([entry(r#"{"MESSAGE":""}"#)]), [""]);
        assert!(lines([entry(r#"{"__CURSOR":"s=1"}"#)]).is_empty());
        assert_eq!(lines([entry("not json")]), ["not json"]);
    }
}
