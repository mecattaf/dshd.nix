//! Malformed frames, connection refusal and native transport identity.

use super::*;

// catches: a malformed frame that kills the controller connection or reaches dsh, or an error frame without the JSON-RPC code (P3.3)
#[test]
fn invalid_frames_get_errors_and_relay_continues() -> TestResult {
    let (instance, running) = acp_instance("garbled")?;
    let mut client = Client::attach(&instance, false)?;
    let cases: [(&[u8], i64); 3] = [
        (b"this is not json", -32700),
        (b"[1, 2, 3]", -32600),
        (br#"{"jsonrpc":"2.0"}"#, -32600),
    ];
    for (frame, code) in cases {
        client.send_raw(frame)?;
        let error = client.receive()?;
        assert_eq!(error.pointer("/error/code"), Some(&json!(code)), "{error}");
        assert_eq!(error.get("id"), Some(&Value::Null), "{error}");
    }
    let (unknown, _) = client.call(9, "dshd/unknown", &json!({}))?;
    assert_eq!(
        unknown.pointer("/error/code"),
        Some(&json!(-32601)),
        "dsh answers what it does not know: {unknown}"
    );
    initialize(&mut client, 10)?;
    let attached = probe(&instance)?;
    assert!(attached.attached_since.is_some(), "{attached:?}");
    let expected = dshd::relay::protocol::ProbeLine {
        attached: true,
        attached_since: attached.attached_since,
        initialized: true,
        held_sessions: Vec::new(),
        pending_permissions: 0,
    };
    assert_eq!(attached, expected);
    assert_eq!(
        methods(&instance.stub_log()?),
        ["dshd/unknown", "initialize"],
        "nothing malformed reached dsh"
    );
    drop(client);
    assert_eq!(running.end(true)?.status, Some(0));
    Ok(())
}

// catches: a client that hangs or reports success when no relay listens, or that guesses a runtime root it was not given (P3.2)
#[test]
fn probe_and_attach_without_runner_are_not_running() -> TestResult {
    let (instance, running) = acp_instance("gone")?;
    assert_eq!(running.end(true)?.status, Some(0));

    let stopped = dshd::error::Error::NotRunning("gone".to_owned());
    let probed = instance.runner("probe").output()?;
    assert_eq!(
        probed.status.code(),
        Some(i32::from(stopped.exit_code())),
        "{}",
        String::from_utf8_lossy(&probed.stderr)
    );
    assert!(String::from_utf8_lossy(&probed.stderr).contains(&stopped.to_string()));
    let attached = instance.runner("attach").output()?;
    assert_eq!(
        attached.status.code(),
        Some(i32::from(stopped.exit_code())),
        "{}",
        String::from_utf8_lossy(&attached.stderr)
    );
    let rootless = instance
        .runner("probe")
        .env_remove("XDG_RUNTIME_DIR")
        .output()?;
    let unavailable = dshd::error::Error::SystemdUnavailable(String::new()).exit_code();
    assert_eq!(
        rootless.status.code(),
        Some(i32::from(unavailable)),
        "{}",
        String::from_utf8_lossy(&rootless.stderr)
    );
    Ok(())
}
