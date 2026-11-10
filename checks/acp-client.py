"""A scripted ACP controller for the dshd VM checks, run as the test user.

acp-client NAME new MODE
    Attaches through `dshd attach NAME`, initializes, opens a session,
    prompts for something that needs a permission and ends at the
    permission request without answering it: MODE `clean` closes the relay
    client's stdin, MODE `abrupt` kills the relay client with SIGKILL.
acp-client NAME resume SESSION
    Attaches, initializes, resumes SESSION, answers the held permission
    request, prompts again and waits for the answer to that prompt.

Prints one JSON object on stdout: the session id and every frame received.
"""

import json
import os
import signal
import subprocess
import sys

# Request ids of a resuming controller start here, so they never collide
# with the ids of the controller that opened the session.
RESUME_FIRST_ID = 1000


class Controller:
    """One `dshd attach` relay client and the frames it delivered."""

    def __init__(self, name, first_id):
        self.client = subprocess.Popen(
            ["dshd", "attach", name],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
        )
        self.frames = []
        self.next_id = first_id

    def send(self, message):
        message["jsonrpc"] = "2.0"
        self.client.stdin.write((json.dumps(message) + "\n").encode())
        self.client.stdin.flush()

    def request(self, method, params):
        self.next_id += 1
        self.send({"id": self.next_id, "method": method, "params": params})
        return self.next_id

    def until(self, match):
        for line in self.client.stdout:
            frame = json.loads(line)
            self.frames.append(frame)
            if match(frame):
                return frame
        raise SystemExit("relay closed: " + json.dumps(self.frames))

    def call(self, method, params):
        ident = self.request(method, params)

        def answers(frame):
            return frame.get("id") == ident and "method" not in frame

        frame = self.until(answers)
        if "error" in frame:
            raise SystemExit(method + " failed: " + json.dumps(frame))
        return frame["result"]


def is_permission(frame):
    return frame.get("method") == "session/request_permission"


def prompt(session, text):
    return {"sessionId": session, "prompt": [{"type": "text", "text": text}]}


def main():
    name, action, argument = sys.argv[1:4]
    first_id = 0 if action == "new" else RESUME_FIRST_ID
    acp = Controller(name, first_id)
    acp.call("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
    where = {"cwd": os.getcwd(), "mcpServers": []}
    if action == "new":
        session = acp.call("session/new", where)["sessionId"]
        acp.request("session/prompt", prompt(session, "ask for permission"))
        acp.until(is_permission)
        if argument == "abrupt":
            acp.client.send_signal(signal.SIGKILL)
        else:
            acp.client.stdin.close()
    else:
        session = argument
        acp.call("session/resume", dict(where, sessionId=session))
        held = acp.until(is_permission)
        options = held["params"].get("options") or [{"optionId": "allow"}]
        outcome = {"outcome": "selected", "optionId": options[0]["optionId"]}
        acp.send({"id": held["id"], "result": {"outcome": outcome}})
        acp.call("session/prompt", prompt(session, "and once more"))
        acp.client.stdin.close()
    acp.client.wait()
    print(json.dumps({"sessionId": session, "frames": acp.frames}))


main()
