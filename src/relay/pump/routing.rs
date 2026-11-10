//! JSON-RPC routing and held-session reattachment within the frame pumps.

use super::{
    AGENT_EXITED, ChildRequest, INTERNAL_ERROR, INVALID_PARAMS, INVALID_REQUEST, InFlight,
    METHOD_INITIALIZE, METHOD_REQUEST_PERMISSION, METHOD_SESSION_CLOSE, METHOD_SESSION_LIST,
    METHOD_SESSION_NEW, METHOD_SESSION_RESUME, Map, Outgoing, PARSE_ERROR, Shared, State, Tracked,
    Value, error_frame, lock, log_line, mcp_servers, object, put_frame, resumed_frame, same_dir,
    send, session_of, text_param,
};

/// Routes one frame from dsh.
pub(super) fn from_child(shared: &Shared, frame: Vec<u8>) {
    let mut out = lock(&shared.out);
    let outgoing = route_child(&mut lock(&shared.state), frame);
    send(shared, &mut out, outgoing);
}

/// Decides where a frame from dsh goes and what the relay learns from it.
fn route_child(state: &mut State, frame: Vec<u8>) -> Option<Outgoing> {
    let Some(mut message) = object(&frame) else {
        return Some(Outgoing::live(frame));
    };
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (method, message.remove("id").filter(|id| !id.is_null())) {
        (Some(method), Some(id)) => {
            let session = session_of(&message);
            let parked = session
                .as_deref()
                .is_some_and(|session| state.held.holds(session) && !state.held.is_bound(session));
            let permission = method == METHOD_REQUEST_PERMISSION;
            state.waiting.push(ChildRequest {
                id,
                session,
                permission,
                frame: frame.clone(),
            });
            (!parked).then(|| Outgoing::live(frame))
        }
        (Some(_), None) => child_notification(state, &message, frame),
        (None, Some(id)) => child_response(state, &id, message),
        (None, None) => Some(Outgoing::live(frame)),
    }
}

/// A notification of a held session goes to the controller that has it
/// bound, or to its ring.
fn child_notification(
    state: &mut State,
    message: &Map<String, Value>,
    frame: Vec<u8>,
) -> Option<Outgoing> {
    let Some(session) = session_of(message).filter(|session| state.held.holds(session)) else {
        return Some(Outgoing::live(frame));
    };
    let options = message
        .get("params")
        .and_then(|params| params.get("update"))
        .and_then(|update| update.get("configOptions"));
    if let Some(options) = options {
        state.held.set_config_options(&session, options.clone());
    }
    if state.held.is_bound(&session) {
        Some(Outgoing {
            frame,
            ring: Some(session),
        })
    } else {
        state.held.buffer(&session, frame);
        None
    }
}

/// dsh's answer to a forwarded request: learnt from, then mapped back to
/// the controller's id, or discarded when that controller is gone.
fn child_response(
    state: &mut State,
    id: &Value,
    mut message: Map<String, Value>,
) -> Option<Outgoing> {
    let flight = id.as_u64().and_then(|id| state.in_flight.remove(&id))?;
    let current = state
        .attached
        .as_ref()
        .is_some_and(|attached| attached.generation == flight.generation);
    if let Some(result) = message.get_mut("result") {
        learn(state, &flight.tracked, current, result);
    }
    current.then(|| {
        message.insert("id".to_owned(), flight.id);
        Outgoing::live(Value::Object(message).to_string().into_bytes())
    })
}

/// Keeps `Held` current from a successful answer.
fn learn(state: &mut State, tracked: &Tracked, current: bool, result: &mut Value) {
    match tracked {
        Tracked::Initialize => state.initialized = true,
        Tracked::Open {
            session,
            cwd,
            mcp_servers,
        } => {
            let answered = result
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if let Some(opened) = session.clone().or(answered) {
                state.held.hold(&opened, cwd);
                state.held.set_mcp_servers(&opened, mcp_servers.clone());
                if let Some(options) = result.get("configOptions") {
                    state.held.set_config_options(&opened, options.clone());
                }
                state.held.bind(&opened, current);
            }
        }
        Tracked::Close(session) => {
            state.held.close(session);
            state
                .waiting
                .retain(|request| request.session.as_ref() != Some(session));
        }
        Tracked::List { cwd } if current => {
            state.held.prepend_to_list(
                |held| cwd.as_deref().is_none_or(|cwd| same_dir(cwd, held)),
                result,
            );
        }
        Tracked::List { .. } | Tracked::Other => {}
    }
}

/// Routes one frame from the attached controller.
pub(super) fn from_controller(shared: &Shared, generation: u64, frame: &[u8]) {
    let mut message = match serde_json::from_slice::<Value>(frame) {
        Ok(Value::Object(message)) => message,
        Ok(_) => {
            let error = error_frame(
                &Value::Null,
                INVALID_REQUEST,
                "a frame must be a JSON-RPC object",
            );
            return reply(shared, generation, error);
        }
        Err(error) => {
            let error = error_frame(&Value::Null, PARSE_ERROR, &format!("parse error: {error}"));
            return reply(shared, generation, error);
        }
    };
    let method = message
        .get("method")
        .and_then(Value::as_str)
        .map(str::to_owned);
    match (method, message.remove("id").filter(|id| !id.is_null())) {
        (Some(method), Some(id)) => controller_request(shared, generation, &method, &id, message),
        (Some(_), None) => {
            to_child(shared, frame);
        }
        (None, Some(id)) => answer(shared, &id, frame),
        (None, None) => {
            let error = error_frame(
                &Value::Null,
                INVALID_REQUEST,
                "a frame must be a request, a notification or a response",
            );
            reply(shared, generation, error);
        }
    }
}

/// A controller request: the resume of a held session is answered here;
/// everything else goes to dsh under a relay id.
fn controller_request(
    shared: &Shared,
    generation: u64,
    method: &str,
    id: &Value,
    mut message: Map<String, Value>,
) {
    let resumed = if method == METHOD_SESSION_RESUME {
        session_of(&message)
    } else {
        None
    };
    if let Some(session) = resumed.filter(|session| lock(&shared.state).held.holds(session)) {
        return rejoin(shared, generation, id, &session, &message);
    }
    let tracked = track(method, &message);
    let relay_id = {
        let mut state = lock(&shared.state);
        state.next_id += 1;
        let relay_id = state.next_id;
        state.in_flight.insert(
            relay_id,
            InFlight {
                generation,
                id: id.clone(),
                tracked,
            },
        );
        relay_id
    };
    message.insert("id".to_owned(), Value::from(relay_id));
    if !to_child(shared, Value::Object(message).to_string().as_bytes()) {
        let unanswered = lock(&shared.state).in_flight.remove(&relay_id).is_some();
        if unanswered {
            reply(
                shared,
                generation,
                error_frame(id, INTERNAL_ERROR, AGENT_EXITED),
            );
        }
    }
}

fn track(method: &str, message: &Map<String, Value>) -> Tracked {
    let params = message.get("params");
    match method {
        METHOD_INITIALIZE => Tracked::Initialize,
        METHOD_SESSION_NEW | METHOD_SESSION_RESUME => Tracked::Open {
            session: if method == METHOD_SESSION_RESUME {
                session_of(message)
            } else {
                None
            },
            cwd: text_param(params, "cwd"),
            mcp_servers: mcp_servers(params),
        },
        METHOD_SESSION_CLOSE => session_of(message).map_or(Tracked::Other, Tracked::Close),
        METHOD_SESSION_LIST
            if params
                .and_then(|params| params.get("cursor"))
                .is_none_or(Value::is_null) =>
        {
            let cwd = params
                .and_then(|params| params.get("cwd"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            Tracked::List { cwd }
        }
        _ => Tracked::Other,
    }
}

/// Answers the resume of a held session, which dsh would reject since the
/// session is active on the relay's connection: with the same cwd and MCP
/// servers, the latest config options and the number of frames the ring
/// dropped since the last resume, then the ring, then dsh's pending
/// requests for it, and the session is bound to this controller.
fn rejoin(
    shared: &Shared,
    generation: u64,
    id: &Value,
    session: &str,
    message: &Map<String, Value>,
) {
    let params = message.get("params");
    let cwd = text_param(params, "cwd");
    let servers = mcp_servers(params);
    let mut out = lock(&shared.out);
    if out
        .as_ref()
        .is_none_or(|controller| controller.generation != generation)
    {
        return;
    }
    let frames = {
        let mut state = lock(&shared.state);
        let terms = state.held.rejoin_terms(session);
        let accepted = terms
            .filter(|terms| same_dir(&cwd, terms.cwd) && servers == *terms.mcp_servers)
            .map(|terms| terms.config_options.cloned());
        if let Some(config_options) = accepted {
            let (ring, dropped) = state.held.drain(session);
            if dropped > 0 {
                log_line(&format!(
                    "a controller resumed {session}; {dropped} of its frames were dropped while it was away"
                ));
            }
            let mut frames = vec![Outgoing::live(resumed_frame(
                id,
                config_options.as_ref(),
                dropped,
            ))];
            frames.extend(ring.into_iter().map(|frame| Outgoing {
                frame,
                ring: Some(session.to_owned()),
            }));
            let pending = state
                .waiting
                .iter()
                .filter(|request| request.session.as_deref() == Some(session));
            frames.extend(pending.map(|request| Outgoing::live(request.frame.clone())));
            state.held.bind(session, true);
            frames
        } else {
            let detail =
                "a held session resumes only with the cwd and mcpServers it was opened with";
            vec![Outgoing::live(error_frame(id, INVALID_PARAMS, detail))]
        }
    };
    send(shared, &mut out, frames);
}

/// A controller's answer to a request from dsh: the first one goes to dsh,
/// later ones are dropped.
fn answer(shared: &Shared, id: &Value, frame: &[u8]) {
    let mut state = lock(&shared.state);
    let first = state
        .waiting
        .iter()
        .position(|request| request.id == *id)
        .map(|index| state.waiting.remove(index));
    drop(state);
    if first.is_some() {
        to_child(shared, frame);
    }
}

/// Sends a frame of the relay's own to the controller of `generation`.
pub(super) fn reply(shared: &Shared, generation: u64, frame: Vec<u8>) {
    let mut out = lock(&shared.out);
    if out
        .as_ref()
        .is_some_and(|controller| controller.generation == generation)
    {
        send(shared, &mut out, Some(Outgoing::live(frame)));
    }
}

/// Writes a frame to dsh's stdin; `false` once it is closed or gone.
pub(super) fn to_child(shared: &Shared, frame: &[u8]) -> bool {
    let mut child_in = lock(&shared.child_in);
    let Some(stdin) = child_in.as_mut() else {
        return false;
    };
    match put_frame(stdin, frame) {
        Ok(()) => true,
        Err(error) => {
            log_line(&format!("cannot write to dsh: {error}"));
            false
        }
    }
}
