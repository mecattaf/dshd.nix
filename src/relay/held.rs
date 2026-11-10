//! Held sessions and their bounded buffers: what the relay remembers
//! between controllers. Pure data, no I/O, fully unit-tested.
//!
//! A session is held from the moment dsh acknowledges its `session/new` or
//! `session/resume` until dsh acknowledges its `session/close`. While no
//! controller has it bound, its frames go into a ring of at most
//! [`RING_PER_SESSION`] frames, and all rings together keep at most
//! [`RING_TOTAL_BYTES`]; past either cap the oldest frame goes and is counted,
//! both in the session's total and in the hole the next resume announces.

use std::collections::{BTreeMap, VecDeque};
use std::mem;

use serde_json::{Map, Value};

use crate::relay::protocol::HeldSession;
use crate::relay::{RING_PER_SESSION, RING_TOTAL_BYTES};

/// The held sessions and their rings.
#[derive(Debug, Default)]
pub struct Held {
    sessions: BTreeMap<String, Entry>,
    total_bytes: usize,
    next_seq: u64,
}

#[derive(Debug, Default)]
struct Entry {
    cwd: String,
    mcp_servers: Value,
    config_options: Option<Value>,
    bound: bool,
    /// Buffered frames with their arrival number, oldest first.
    frames: VecDeque<(u64, Vec<u8>)>,
    /// Frames dropped since the session was held, as the probe reports it.
    dropped: usize,
    /// Frames dropped since the ring was last drained: the hole the next
    /// resume announces.
    gap: usize,
}

impl Entry {
    fn count_drop(&mut self) {
        self.dropped += 1;
        self.gap += 1;
    }
}

/// What a controller resuming a held session must match, and what the
/// relay answers it with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RejoinTerms<'a> {
    /// The cwd the session was opened with.
    pub(crate) cwd: &'a str,
    /// The `mcpServers` it was opened with.
    pub(crate) mcp_servers: &'a Value,
    /// The latest `configOptions` dsh reported for it.
    pub(crate) config_options: Option<&'a Value>,
}

impl Held {
    /// Records a session the child created or resumed.
    pub fn hold(&mut self, session_id: &str, cwd: &str) {
        let entry = self.sessions.entry(session_id.to_owned()).or_default();
        cwd.clone_into(&mut entry.cwd);
    }

    /// Buffers a frame for a session while no controller is attached,
    /// dropping the oldest frames past the per-session and total caps. A
    /// frame larger than the whole budget is dropped itself.
    pub fn buffer(&mut self, session_id: &str, frame: Vec<u8>) {
        let Some(entry) = self.sessions.get_mut(session_id) else {
            return;
        };
        if frame.len() > RING_TOTAL_BYTES {
            entry.count_drop();
            return;
        }
        if entry.frames.len() >= RING_PER_SESSION
            && let Some((_, oldest)) = entry.frames.pop_front()
        {
            self.total_bytes = self.total_bytes.saturating_sub(oldest.len());
            entry.count_drop();
        }
        self.total_bytes += frame.len();
        entry.frames.push_back((self.next_seq, frame));
        self.next_seq += 1;
        while self.total_bytes > RING_TOTAL_BYTES && self.drop_oldest() {}
    }

    /// Drains the buffered frames of a session for a controller that
    /// resumed it, with the number of frames dropped since the last drain,
    /// which the resume answer announces; that count starts again at zero.
    pub fn drain(&mut self, session_id: &str) -> (Vec<Vec<u8>>, usize) {
        let Some(entry) = self.sessions.get_mut(session_id) else {
            return (Vec::new(), 0);
        };
        let frames: Vec<Vec<u8>> = entry.frames.drain(..).map(|(_, frame)| frame).collect();
        let bytes: usize = frames.iter().map(Vec::len).sum();
        self.total_bytes = self.total_bytes.saturating_sub(bytes);
        (frames, mem::take(&mut entry.gap))
    }

    /// Marks a session bound or unbound.
    pub fn bind(&mut self, session_id: &str, bound: bool) {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.bound = bound;
        }
    }

    /// Unbinds every session (a controller left).
    pub fn unbind_all(&mut self) {
        for entry in self.sessions.values_mut() {
            entry.bound = false;
        }
    }

    /// Forgets a closed session.
    pub fn close(&mut self, session_id: &str) {
        if let Some(entry) = self.sessions.remove(session_id) {
            let bytes: usize = entry.frames.iter().map(|(_, frame)| frame.len()).sum();
            self.total_bytes = self.total_bytes.saturating_sub(bytes);
        }
    }

    /// Whether the relay holds this session.
    pub fn holds(&self, session_id: &str) -> bool {
        self.sessions.contains_key(session_id)
    }

    /// The report rows.
    pub fn report(&self) -> Vec<HeldSession> {
        self.sessions
            .iter()
            .map(|(session_id, entry)| HeldSession {
                session_id: session_id.clone(),
                cwd: entry.cwd.clone(),
                bound: entry.bound,
                buffered: entry.frames.len(),
                dropped: entry.dropped,
            })
            .collect()
    }

    /// Whether the attached controller has this session bound.
    pub(crate) fn is_bound(&self, session_id: &str) -> bool {
        self.sessions
            .get(session_id)
            .is_some_and(|entry| entry.bound)
    }

    /// Records the `mcpServers` a held session was opened with.
    pub(crate) fn set_mcp_servers(&mut self, session_id: &str, mcp_servers: Value) {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.mcp_servers = mcp_servers;
        }
    }

    /// Records the latest `configOptions` dsh reported for a held session.
    pub(crate) fn set_config_options(&mut self, session_id: &str, config_options: Value) {
        if let Some(entry) = self.sessions.get_mut(session_id) {
            entry.config_options = Some(config_options);
        }
    }

    /// The terms of a held session, or `None` when it is not held.
    pub(crate) fn rejoin_terms(&self, session_id: &str) -> Option<RejoinTerms<'_>> {
        self.sessions.get(session_id).map(|entry| RejoinTerms {
            cwd: &entry.cwd,
            mcp_servers: &entry.mcp_servers,
            config_options: entry.config_options.as_ref(),
        })
    }

    /// Puts the held sessions whose cwd `matches` first in a
    /// `session/list` result, in dsh's own `{sessionId, cwd}` shape, and
    /// drops dsh's rows for them; dsh does not list sessions in use.
    pub(crate) fn prepend_to_list(&self, matches: impl Fn(&str) -> bool, result: &mut Value) {
        let Some(Value::Array(listed)) = result.get_mut("sessions") else {
            return;
        };
        let held: Vec<(&String, &Entry)> = self
            .sessions
            .iter()
            .filter(|(_, entry)| matches(entry.cwd.as_str()))
            .collect();
        listed.retain(|row| {
            let id = row.get("sessionId").and_then(Value::as_str);
            !held
                .iter()
                .any(|(session_id, _)| id == Some(session_id.as_str()))
        });
        let mut rows: Vec<Value> = held
            .into_iter()
            .map(|(session_id, entry)| {
                let mut row = Map::new();
                row.insert("sessionId".to_owned(), Value::String(session_id.clone()));
                row.insert("cwd".to_owned(), Value::String(entry.cwd.clone()));
                Value::Object(row)
            })
            .collect();
        rows.append(listed);
        *listed = rows;
    }

    /// Drops the globally oldest buffered frame. `false` when nothing is
    /// buffered.
    fn drop_oldest(&mut self) -> bool {
        let oldest = self
            .sessions
            .values_mut()
            .filter_map(|entry| {
                let seq = entry.frames.front()?.0;
                Some((seq, entry))
            })
            .min_by_key(|(seq, _)| *seq);
        let Some((_, entry)) = oldest else {
            return false;
        };
        let Some((_, frame)) = entry.frames.pop_front() else {
            return false;
        };
        entry.count_drop();
        self.total_bytes = self.total_bytes.saturating_sub(frame.len());
        true
    }
}

#[cfg(test)]
mod tests {
    use super::Held;
    use crate::relay::{RING_PER_SESSION, RING_TOTAL_BYTES};

    fn frame(tag: &str, n: usize) -> Vec<u8> {
        format!("{tag}-{n}").into_bytes()
    }

    fn row(held: &Held, session: &str) -> (usize, usize) {
        let report = held.report();
        let row = report.iter().find(|row| row.session_id == session);
        row.map_or((usize::MAX, usize::MAX), |row| (row.buffered, row.dropped))
    }

    // catches: a ring that grows past 1024 frames or 8 MiB, or drops the newest instead of the oldest (P1.4)
    #[test]
    fn per_session_and_total_bounds() {
        let mut held = Held::default();
        held.hold("a", "/w");
        let extra = 3;
        for n in 0..RING_PER_SESSION + extra {
            held.buffer("a", frame("a", n));
        }
        assert_eq!(row(&held, "a"), (RING_PER_SESSION, extra));
        let (drained, hole) = held.drain("a");
        assert_eq!(hole, extra);
        assert_eq!(drained.first(), Some(&frame("a", extra)));
        assert_eq!(
            drained.last(),
            Some(&frame("a", RING_PER_SESSION + extra - 1))
        );

        let quarter = RING_TOTAL_BYTES / 4;
        let big = |byte: u8| vec![byte; quarter];
        held.hold("b", "/w");
        held.buffer("a", big(b'1'));
        held.buffer("b", big(b'2'));
        held.buffer("a", big(b'3'));
        held.buffer("b", big(b'4'));
        assert_eq!(
            (row(&held, "a").0, row(&held, "b").0),
            (2, 2),
            "exactly at the cap nothing is dropped"
        );
        held.buffer("b", big(b'5'));
        assert_eq!(
            row(&held, "a"),
            (1, extra + 1),
            "the globally oldest frame went"
        );
        assert_eq!(row(&held, "b"), (3, 0));
        assert_eq!(held.drain("a"), (vec![big(b'3')], 1));

        held.buffer("a", vec![b'x'; RING_TOTAL_BYTES + 1]);
        assert_eq!(
            row(&held, "a"),
            (0, extra + 2),
            "a frame larger than the total cap is dropped itself"
        );
        assert_eq!(held.drain("b"), (vec![big(b'2'), big(b'4'), big(b'5')], 0));
    }

    // catches: frames drained out of order, counts lost on drain, or state kept for closed or unknown sessions (P1.4, P3.1)
    #[test]
    fn drain_order_and_dropped_count() {
        let mut held = Held::default();
        held.buffer("unknown", frame("u", 0));
        assert!(!held.holds("unknown"), "buffering never creates a session");

        held.hold("s", "/w");
        for n in 0..3 {
            held.buffer("s", frame("s", n));
        }
        assert_eq!(
            held.drain("s"),
            (vec![frame("s", 0), frame("s", 1), frame("s", 2)], 0)
        );
        assert_eq!(row(&held, "s"), (0, 0));
        assert!(held.drain("s").0.is_empty());

        held.bind("s", true);
        assert!(held.is_bound("s"));
        held.hold("t", "/x");
        held.bind("t", true);
        held.unbind_all();
        assert!(held.report().iter().all(|row| !row.bound));

        let terms = held.rejoin_terms("t").map(|terms| terms.cwd.to_owned());
        assert_eq!(terms.as_deref(), Some("/x"));
        held.buffer("t", frame("t", 0));
        held.close("t");
        assert!(!held.holds("t"));
        assert!(held.rejoin_terms("t").is_none());
        assert_eq!(held.report().len(), 1);
    }

    // catches: a resume that hides a hole in the stream: drops not counted since the last drain, a hole announced
    // twice, or the probe's total reset along with it
    #[test]
    fn drain_reports_the_hole_since_the_last_drain() {
        let mut held = Held::default();
        held.hold("s", "/w");
        for n in 0..RING_PER_SESSION + 2 {
            held.buffer("s", frame("s", n));
        }
        let (frames, hole) = held.drain("s");
        assert_eq!((frames.len(), hole), (RING_PER_SESSION, 2));
        assert_eq!(held.drain("s"), (Vec::new(), 0), "a hole is announced once");
        held.buffer("s", frame("s", 0));
        assert_eq!(
            row(&held, "s"),
            (1, 2),
            "the probe keeps the total since the session was held"
        );
        assert_eq!(held.drain("unknown"), (Vec::new(), 0));
    }

    // catches: a session/list answer that omits held sessions, lists one twice, or ignores the cwd filter (P3.1)
    #[test]
    fn list_puts_matching_held_sessions_first_without_duplicates() -> Result<(), serde_json::Error>
    {
        let mut held = Held::default();
        held.hold("s", "/w");
        held.hold("t", "/x");
        let listed =
            r#"{"sessions":[{"sessionId":"other","cwd":"/w"},{"sessionId":"s","cwd":"/w"}]}"#;
        let mut result: serde_json::Value = serde_json::from_str(listed)?;
        held.prepend_to_list(|cwd| cwd == "/w", &mut result);
        let expected =
            r#"{"sessions":[{"sessionId":"s","cwd":"/w"},{"sessionId":"other","cwd":"/w"}]}"#;
        assert_eq!(result, serde_json::from_str::<serde_json::Value>(expected)?);

        let mut unfiltered: serde_json::Value = serde_json::from_str(r#"{"sessions":[]}"#)?;
        held.prepend_to_list(|_| true, &mut unfiltered);
        let rows = unfiltered
            .get("sessions")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten();
        let ids: Vec<_> = rows.filter_map(|row| row.get("sessionId")).collect();
        assert_eq!(ids, ["s", "t"]);

        let mut failed: serde_json::Value = serde_json::from_str(r#"{"error":"none"}"#)?;
        held.prepend_to_list(|_| true, &mut failed);
        assert_eq!(
            failed,
            serde_json::from_str::<serde_json::Value>(r#"{"error":"none"}"#)?,
            "a result without a list is left alone"
        );
        Ok(())
    }
}
