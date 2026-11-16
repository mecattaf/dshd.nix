# `launchctl print` fixtures

Text in the shape `launchctl print` and `launchctl print-disabled` print on
macOS 14 and 15, written from the documented and commonly published format.
They are not yet captures from a real Mac: the real-Mac session in the
dshd macOS test plan captures the same four outputs for a throwaway job and
replaces these files where they differ. The parsers must turn any line they
do not recognise into an unknown fact, never a wrong one.

- `job-running.txt`: a running job (`state = running`, a `pid`).
- `job-exited-0.txt`: a job loaded but not running after a clean exit.
- `job-exited-78.txt`: a job whose runner exited 78.
- `job-killed.txt`: a job whose runner was killed by SIGKILL.
- `job-never-ran.txt`: a job loaded but never spawned.
- `job-odd-state.txt`: a state word this version does not know.
- `domain.txt`: `launchctl print gui/501`, the services of the domain.
- `disabled.txt`: `launchctl print-disabled gui/501` (macOS 14 words).
- `disabled-legacy.txt`: the same with the older `true`/`false` words.
