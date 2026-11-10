//! Ordered shutdown and native cancellation over the ACP relay.

use super::*;

// catches: a stop that closes dsh's stdin with a permission still unanswered, or a detach that forgets a permission it was shown (P3.1)
#[test]
fn stop_never_leaves_a_permission_unanswered() -> TestResult {
    let (instance, running) = acp_instance("stopping")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut client = Client::attach(&instance, false)?;
    initialize(&mut client, 1)?;
    let session = open(&mut client, 2, &cwd)?;
    client.send(&prompt(3, &session, "permission to stop?"))?;
    let (asked, _) = client.until(asks_permission)?;
    assert_eq!(client.detach()?, Some(0));
    wait_for("the relay to see the detach", || {
        Ok(!probe(&instance)?.attached)
    })?;
    let parked = dshd::relay::protocol::ProbeLine {
        attached: false,
        attached_since: None,
        initialized: true,
        held_sessions: vec![row(&session, &cwd, false, 0)?],
        pending_permissions: 1,
    };
    assert_eq!(probe(&instance)?, parked, "the detach answered nothing");
    assert!(answers(&instance.stub_log()?).is_empty());

    let ended = running.end(true)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let id = asked.get("id").ok_or("no id")?;
    assert_eq!(answers(&instance.stub_log()?), [format!("{id} cancelled")]);
    Ok(())
}

// catches: a stop that tells the controller 'agent exited' for a turn dsh then answers, or drops dsh's last frames
#[test]
fn stop_lets_dsh_answer_before_the_controller_is_closed() -> TestResult {
    let (instance, running) = acp_instance("draining")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut client = Client::attach(&instance, false)?;
    initialize(&mut client, 1)?;
    let session = open(&mut client, 2, &cwd)?;
    client.send(&prompt(3, &session, "permission, then stop"))?;
    client.until(asks_permission)?;

    let ended = running.end(true)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    let (last_words, _) = client.until(|frame| text_of(frame).is_some())?;
    assert_eq!(
        text_of(&last_words),
        Some("permission cancelled"),
        "{last_words}"
    );
    let answer = client.receive()?;
    assert_eq!(
        answer.get("id"),
        Some(&json!(3)),
        "dsh's answer, not the relay's: {answer}"
    );
    assert_eq!(
        answer.pointer("/result/stopReason"),
        Some(&json!("cancelled")),
        "{answer}"
    );
    assert!(answer.get("error").is_none(), "{answer}");
    assert!(
        client.closed(),
        "the controller gets EOF after dsh's last frame"
    );
    Ok(())
}

// catches: a session/cancel swallowed by the relay or answered by it, so the turn never ends cancelled (P3.1)
#[test]
fn cancel_is_forwarded_not_answered() -> TestResult {
    let (instance, running) = acp_instance("cancels")?;
    let cwd = cwd_of(&instance, "work")?;
    let mut client = Client::attach(&instance, false)?;
    initialize(&mut client, 1)?;
    let session = open(&mut client, 2, &cwd)?;
    client.send(&prompt(3, &session, "permission, then cancel"))?;
    client.until(asks_permission)?;
    let cancel = "session/cancel";
    client
        .send(&json!({ "jsonrpc": "2.0", "method": cancel, "params": { "sessionId": session } }))?;
    let (ended, before) = client.until(|frame| frame.get("id") == Some(&json!(3)))?;
    assert_eq!(
        ended.pointer("/result/stopReason"),
        Some(&json!("cancelled")),
        "{ended}"
    );
    assert!(
        before.iter().all(|frame| method_of(frame).is_some()),
        "the relay answered the notification: {before:?}"
    );
    assert!(
        methods(&instance.stub_log()?).contains(&cancel),
        "the cancel reached dsh"
    );
    drop(client);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}

/// A real blocked pipe writer proves the stop races a wedged ACP stdin, without a sleep.
fn pipe_writer_waits(runner: u32) -> TestResult<bool> {
    for task in std::fs::read_dir(format!("/proc/{runner}/task"))? {
        let path = task?.path().join("wchan");
        if std::fs::read_to_string(path).is_ok_and(|state| state.contains("pipe_write")) {
            return Ok(true);
        }
    }
    Ok(false)
}

// catches: stop blocking behind a controller's full write to dsh's wedged stdin, preventing main-thread reaping
#[test]
fn a_wedged_child_stdin_uses_the_supervisors_signal_fallback() -> TestResult {
    let instance = Instance::create(
        "wedged",
        &Spec {
            profile: "acp",
            args: &[],
            env: &[("DSH_STUB_MODE", "acp-stalled-input")],
        },
    )?;
    let running = instance.start()?;
    wait_for("the relay socket", || {
        Ok(instance.runtime().acp_socket().exists())
    })?;
    let mut client = Client::attach(&instance, false)?;
    let (release, hold) = mpsc::channel::<()>();
    let sending = thread::spawn(move || {
        let sent = client.send(&json!({ "jsonrpc": "2.0", "id": 1, "method": "session/prompt", "params": { "text": "x".repeat(2 * 1024 * 1024) } })).is_ok();
        let _released = hold.recv();
        sent
    });
    let child: u32 = instance
        .stub_log()?
        .lines()
        .find_map(|line| line.strip_prefix("invocation "))
        .ok_or("no child pid")?
        .parse()?;
    let status = std::fs::read_to_string(format!("/proc/{child}/status"))?;
    let runner: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("PPid:"))
        .ok_or("no runner pid")?
        .trim()
        .parse()?;
    wait_for("the controller to fill the child's stdin", || {
        pipe_writer_waits(runner)
    })?;
    let ended = running.end(true)?;
    assert_eq!(ended.status, Some(0), "{}", ended.journal);
    drop(release);
    assert!(sending.join().is_ok(), "the test client writer terminated");
    assert!(
        std::fs::metadata(instance.runtime().path().join("child-end.json")).is_ok(),
        "the supervisor reaped and recorded dsh"
    );
    Ok(())
}
