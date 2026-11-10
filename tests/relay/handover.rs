//! Controller replacement, retained native sessions and permission handover.

use super::*;

/// Answers to a controller that was replaced must never appear on its successor's connection.
fn assert_no_stale_answers(frames: &[Value]) {
    for frame in frames.iter().filter(|frame| method_of(frame).is_none()) {
        assert!(
            [1001, 1002, 1003]
                .iter()
                .any(|id| frame.get("id") == Some(&json!(id))),
            "a replaced controller's answer reached its successor: {frame}"
        );
    }
}

// catches: a controller killed mid-turn taking the turn's later updates with it, or a second controller that
// cannot resume the held session (P1.4, P3.1)
#[test]
fn disconnected_controller_mid_turn_leaves_updates_for_reattach() -> TestResult {
    let (instance, running) = acp_instance("churn")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut first = Client::attach(&instance, false)?;
    initialize(&mut first, 1)?;
    let session = open(&mut first, 2, &cwd)?;
    first.send(&prompt(3, &session, "wait at the gate"))?;
    first.until(|frame| text_of(frame) == Some("turn 1"))?;
    first.kill()?;
    wait_for("the relay to drop the controller", || {
        Ok(!probe(&instance)?.attached)
    })?;

    std::fs::write(instance.path(common::GATE), "")?;
    let parked = row(&session, &cwd, false, 1)?;
    wait_for("the update to be buffered", || {
        Ok(probe(&instance)?.held_sessions == [parked.clone()])
    })?;

    let mut second = Client::attach(&instance, false)?;
    let init = initialize(&mut second, 1001)?;
    assert_eq!(
        init.pointer("/result/_meta/initializeCount"),
        Some(&json!(2)),
        "a second initialize reaches dsh: {init}"
    );
    let resumed = resume(&mut second, 1002, &session, &cwd, &json!([]))?;
    assert_eq!(
        resumed.pointer("/result/configOptions/0/currentValue"),
        Some(&json!("default")),
        "{resumed}"
    );
    let (update, _) = second.until(|frame| text_of(frame).is_some())?;
    assert_eq!(
        text_of(&update),
        Some("after gate 1"),
        "the buffered update comes right after the resume"
    );
    assert_eq!(
        probe(&instance)?.held_sessions,
        [row(&session, &cwd, true, 0)?],
        "the ring is drained into the resume"
    );

    second.send(&prompt(1003, &session, "carry on"))?;
    let (answer, before) = second.until(|frame| frame.get("id") == Some(&json!(1003)))?;
    assert_no_stale_answers(&before);
    assert_eq!(
        answer.pointer("/result/stopReason"),
        Some(&json!("end_turn"))
    );
    assert!(
        before.iter().any(|frame| text_of(frame) == Some("turn 2")),
        "{before:?}"
    );
    let log = instance.stub_log()?;
    assert!(
        !methods(&log).contains(&dshd::relay::protocol::METHOD_SESSION_RESUME),
        "the relay answered the resume itself"
    );
    assert_eq!(
        methods(&log),
        [
            "initialize",
            "session/new",
            "session/prompt",
            "initialize",
            "session/prompt"
        ]
    );
    drop(second);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}

// catches: two controllers writing to one dsh, a refusal reported as success, or a takeover that leaves the old controller connected (P3.3)
#[test]
fn second_attach_refused_until_takeover() -> TestResult {
    let (instance, running) = acp_instance("contested")?;
    let mut first = Client::attach(&instance, false)?;
    initialize(&mut first, 1)?;

    let (code, stderr) = Client::attach(&instance, false)?.refused()?;
    let refusal = dshd::error::Error::AttachedElsewhere("contested".to_owned());
    assert_eq!(code, Some(i32::from(refusal.exit_code())), "{stderr}");
    assert!(stderr.contains(&refusal.to_string()), "{stderr}");
    assert!(probe(&instance)?.attached);

    let mut third = Client::attach(&instance, true)?;
    let init = initialize(&mut third, 1)?;
    assert_eq!(
        init.pointer("/result/_meta/initializeCount"),
        Some(&json!(2)),
        "{init}"
    );
    assert_eq!(
        first.exit()?,
        Some(0),
        "the replaced controller's connection ends"
    );
    assert!(
        first.receive().is_err(),
        "nothing reaches the replaced controller"
    );
    drop(third);

    let ended = running.end(true)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    Ok(())
}

// catches: a permission request lost with the controller that saw it, answered by the relay on its own, or answered twice (P3.1)
#[test]
fn permission_resent_after_takeover_never_auto_answered() -> TestResult {
    let (instance, running) = acp_instance("asks")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut first = Client::attach(&instance, false)?;
    initialize(&mut first, 1)?;
    let session = open(&mut first, 2, &cwd)?;
    first.send(&prompt(3, &session, "this needs permission"))?;
    let (asked, _) = first.until(asks_permission)?;
    let permission = asked
        .get("id")
        .cloned()
        .ok_or("the permission request has no id")?;
    let report = probe(&instance)?;
    let asking = dshd::relay::protocol::ProbeLine {
        attached: true,
        attached_since: report.attached_since,
        initialized: true,
        held_sessions: vec![row(&session, &cwd, true, 0)?],
        pending_permissions: 1,
    };
    assert_eq!(report, asking);

    let mut second = Client::attach(&instance, true)?;
    initialize(&mut second, 1001)?;
    assert_eq!(
        probe(&instance)?.pending_permissions,
        1,
        "a takeover answers nothing"
    );
    resume(&mut second, 1002, &session, &cwd, &json!([]))?;
    let (again, _) = second.until(asks_permission)?;
    assert_eq!(
        again.get("id"),
        Some(&permission),
        "dsh's own id travels unchanged"
    );
    assert_eq!(
        pump::session_id_of(again.to_string().as_bytes()).as_deref(),
        Some(session.as_str())
    );
    assert_eq!(
        probe(&instance)?.held_sessions,
        [row(&session, &cwd, true, 0)?],
        "the takeover's resume binds the session"
    );

    let outcome = json!({ "outcome": "selected", "optionId": "allow" });
    second
        .send(&json!({ "jsonrpc": "2.0", "id": permission, "result": { "outcome": outcome } }))?;
    second.until(|frame| text_of(frame) == Some("permission allow"))?;
    let (_, before) = second.call(1003, "session/list", &json!({}))?;
    assert_no_stale_answers(&before);
    assert_eq!(probe(&instance)?.pending_permissions, 0);
    assert_eq!(
        answers(&instance.stub_log()?),
        [format!("{permission} allow")]
    );
    drop(second);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}

// catches: a held session resumed with another cwd or other MCP servers, or a session/list that hides the
// held session from the next controller (P3.1)
#[test]
fn resume_with_mismatched_cwd_is_rejected() -> TestResult {
    let (instance, running) = acp_instance("handover")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut first = Client::attach(&instance, false)?;
    initialize(&mut first, 1)?;
    let session = open(&mut first, 2, &cwd)?;
    assert_eq!(first.detach()?, Some(0));
    wait_for("the relay to see the detach", || {
        Ok(!probe(&instance)?.attached)
    })?;

    let mut second = Client::attach(&instance, false)?;
    initialize(&mut second, 1)?;
    let (listed, _) = second.call(2, "session/list", &json!({ "cwd": cwd }))?;
    assert_eq!(
        listed.pointer("/result/sessions"),
        Some(&json!([{ "sessionId": session, "cwd": cwd }])),
        "{listed}"
    );

    let elsewhere = cwd_of(&instance, "home")?;
    let moved = resume(&mut second, 3, &session, &elsewhere, &json!([]))?;
    assert_eq!(
        moved.pointer("/error/code"),
        Some(&json!(-32602)),
        "{moved}"
    );
    let server = json!([{ "name": "extra", "command": "/bin/true", "args": [], "env": [] }]);
    let widened = resume(&mut second, 4, &session, &cwd, &server)?;
    assert_eq!(
        widened.pointer("/error/code"),
        Some(&json!(-32602)),
        "{widened}"
    );
    let resumed = resume(&mut second, 5, &session, &cwd, &json!([]))?;
    assert!(resumed.get("result").is_some(), "{resumed}");
    assert_eq!(
        probe(&instance)?.held_sessions,
        [row(&session, &cwd, true, 0)?]
    );
    let resume_method = dshd::relay::protocol::METHOD_SESSION_RESUME;
    assert!(
        !methods(&instance.stub_log()?).contains(&resume_method),
        "the relay answered every resume itself"
    );
    drop(second);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}
