//! Controller stalls, bounded replay and explicit gaps in held output.

use super::*;

/// Completes a raw controller's handshake and opens its first native session.
fn raw_session(stream: &UnixStream, reader: &mut impl BufRead, cwd: &str) -> TestResult<String> {
    let mut writer = stream;
    writer.write_all(b"{\"dshd\":\"attach\"}\n")?;
    let mut reply = String::new();
    reader.read_line(&mut reply)?;
    assert_eq!(
        serde_json::from_str::<HandshakeReply>(&reply)?,
        HandshakeReply::Attached
    );
    raw_call(
        stream,
        reader,
        1,
        "initialize",
        &json!({ "protocolVersion": 1, "clientCapabilities": {} }),
    )?;
    let opened = raw_call(
        stream,
        reader,
        2,
        "session/new",
        &json!({ "cwd": cwd, "mcpServers": [] }),
    )?;
    let session = opened
        .pointer("/result/sessionId")
        .and_then(Value::as_str)
        .ok_or("session/new gave no session")?
        .to_owned();
    Ok(session)
}

// catches: a controller that stops reading blocking dsh, the status probe or the next attach (holder invariant:
// a stalled reader never blocks the program)
#[test]
fn stalled_controller_is_detached_and_its_frames_kept() -> TestResult {
    let (instance, running) = acp_instance("stalled")?;
    let cwd = cwd_of(&instance, "work")?;
    let stream = UnixStream::connect(instance.runtime().acp_socket())?;
    stream.set_read_timeout(Some(FRAME_BOUND))?;
    let mut reader = BufReader::new(&stream);
    let session = raw_session(&stream, &mut reader, &cwd)?;
    pump::write_frame(
        &mut &stream,
        prompt(3, &session, "flood").to_string().as_bytes(),
    )?;
    // From here on nothing reads the connection. The wait is bounded well
    // past CONTROLLER_STALL, and each probe must answer while the relay's
    // write to the stalled controller is blocked.
    wait_for("the relay to detach the stalled controller", || {
        Ok(!probe(&instance)?.attached)
    })?;
    wait_for("dsh to write its whole flood", || {
        Ok(instance.stub_log()?.contains("flooded"))
    })?;
    let held = probe(&instance)?.held_sessions;
    let [kept] = held.as_slice() else {
        return Err(format!("one held session expected: {held:?}").into());
    };
    assert!(
        !kept.bound && kept.buffered > 0,
        "what the stalled controller missed is held: {kept:?}"
    );

    let mut next = Client::attach(&instance, false)?;
    initialize(&mut next, 1001)?;
    let resumed = resume(&mut next, 1002, &session, &cwd, &json!([]))?;
    assert!(resumed.get("result").is_some(), "{resumed}");
    // What dsh wrote after the probe looked is still in order behind the replay.
    next.send(&prompt(1003, &session, "carry on"))?;
    let (answer, before) = next.until(|frame| frame.get("id") == Some(&json!(1003)))?;
    assert_eq!(
        answer.pointer("/result/stopReason"),
        Some(&json!("end_turn")),
        "{answer}"
    );
    assert!(
        before
            .iter()
            .all(|frame| method_of(frame).as_deref() == Some("session/update")),
        "only updates come first"
    );
    let floods: Vec<&str> = before
        .iter()
        .filter_map(text_of)
        .filter(|text| text.starts_with("flood "))
        .collect();
    assert!(
        floods.len() >= kept.buffered,
        "every held frame is replayed: {} of {}",
        floods.len(),
        kept.buffered
    );
    assert!(
        floods
            .last()
            .is_some_and(|text| text.starts_with(&format!("flood {FLOOD} "))),
        "the flood's end reaches the next controller"
    );
    drop(next);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}

// catches: a spliced stream replayed as whole: a resume after frames were dropped that does not say how many
#[test]
fn resume_after_dropped_frames_announces_the_hole() -> TestResult {
    let (instance, running) = acp_instance("holed")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut first = Client::attach(&instance, false)?;
    initialize(&mut first, 1)?;
    let session = open(&mut first, 2, &cwd)?;
    first.send(&prompt(3, &session, "flood after the gate"))?;
    first.until(|frame| text_of(frame) == Some("turn 1"))?;
    assert_eq!(first.detach()?, Some(0));
    wait_for("the relay to see the detach", || {
        Ok(!probe(&instance)?.attached)
    })?;

    std::fs::write(instance.path(common::GATE), "")?;
    let mut held = None;
    wait_for("the flood to be held", || {
        held = probe(&instance)?.held_sessions.into_iter().next();
        Ok(held
            .as_ref()
            .is_some_and(|row| row.buffered + row.dropped == FLOOD))
    })?;
    let held = held.ok_or("no held session")?;
    assert_eq!(
        (held.buffered, held.dropped),
        (RING_PER_SESSION, FLOOD - RING_PER_SESSION),
        "{held:?}"
    );

    let mut second = Client::attach(&instance, false)?;
    initialize(&mut second, 1001)?;
    let resumed = resume(&mut second, 1002, &session, &cwd, &json!([]))?;
    assert_eq!(
        resumed.pointer("/result/_meta/dshd/droppedFrames"),
        Some(&json!(held.dropped)),
        "{resumed}"
    );
    let (oldest, _) = second.until(|frame| text_of(frame).is_some())?;
    let after_hole = format!("flood {} ", held.dropped + 1);
    assert!(
        text_of(&oldest).is_some_and(|text| text.starts_with(&after_hole)),
        "the replay starts after the hole: {oldest}"
    );
    drop(second);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}
