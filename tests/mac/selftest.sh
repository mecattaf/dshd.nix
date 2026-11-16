#!/usr/bin/env bash
# The self-test of tests/mac/acceptance.sh on Linux: the kit runs against
# tests/mac/fake/dshd and the stub dsh, with tests/mac/fake/macos-tools.sh
# standing in for launchctl, uname, codesign and the other macOS commands,
# in a scratch home. It checks the kit's verdicts in each scenario and that
# the kit leaves nothing behind. It proves the kit's logic and clean-up, not
# dshd's behaviour on a Mac.
#
#   tests/mac/selftest.sh
#
# Needs bash, coreutils, procps, util-linux (setsid), jq, lsof and od on
# PATH. Run it inside a private PID namespace when other work shares the
# machine, since it starts and kills processes.

set -u
here=$(cd "$(dirname "$0")" && pwd)
repo=$(cd "$here/../.." && pwd)
kit=$here/acceptance.sh
work=$(mktemp -d /tmp/dshd-kit-selftest.XXXXXX)
home=$work/home
sys=$work/sys
prefix=zzk
failed=0
out=$work/out
trap 'pkill -KILL -f "dshd-release-$prefix/" 2>/dev/null; chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"; rm -rf /tmp/dshd-$prefix.*' EXIT

mkdir -p "$home" "$sys"
for tool in bash sh env cat cp chmod mkdir mktemp rm rmdir ln ls head tail cut tr sed grep awk sort wc date sleep dirname basename readlink ps pgrep pkill id xargs jq od sha256sum nohup setsid touch find kill lsof; do
	path=$(type -P "$tool") || {
		echo "selftest: $tool is not on PATH" >&2
		exit 2
	}
	ln -s "$path" "$sys/$tool"
done
for tool in uname sw_vers hostname xcode-select otool file codesign xattr plutil launchctl; do
	ln -s "$here/fake/macos-tools.sh" "$sys/$tool"
done

kit_run() {
	env -i HOME="$home" USER="$(id -un)" TMPDIR=/tmp PATH="$sys" \
		DSHD_KIT_HOME="$home" DSHD_KIT_SYSTEM_PATH="$sys" \
		bash "$kit" "$@" >"$out" 2>&1
}

kit_common=(--dshd "$here/fake/dshd" --dshd-run "$here/fake/dshd-run" --dsh "$repo/tests/stub/dsh.sh" --prefix "$prefix" --timeout 20)

ok() {
	printf 'ok    %s\n' "$1"
}

bad() {
	printf 'FAIL  %s\n' "$1"
	sed 's/^/      | /' "$out"
	failed=$((failed + 1))
}

# expect SCENARIO STATUS WANTED_STATUS LINE...: each LINE (a progress line
# of the kit, "[STEP] RESULT CHECK") must appear in the output.
expect() {
	local scenario=$1 status=$2 wanted=$3 line
	shift 3
	if [ "$status" != "$wanted" ]; then
		bad "$scenario: exit $status, wanted $wanted"
		return
	fi
	for line in "$@"; do
		if ! grep -q -F -- "$line" "$out"; then
			bad "$scenario: no line '$line'"
			return
		fi
	done
	ok "$scenario"
}

no_fail_rows() {
	if grep -q -E '^\[[0-9a-z]+\] FAIL ' "$out"; then
		bad "$1: a FAIL row: $(grep -E '^\[[0-9a-z]+\] FAIL ' "$out" | head -n 1)"
	else
		ok "$1: no FAIL row"
	fi
}

# Nothing of the prefix is left: no state, no LaunchAgents copy, no fake job,
# no temporary prefix, no process.
left_clean() {
	local rest
	rest=$(
		find "$home/.local" "$home/Library" -name "*$prefix*" 2>/dev/null
		find "$home/.fake-launchd" -name "dshd.$prefix-*" 2>/dev/null
		find "$home/.config/dshd/releases" -name "$prefix" 2>/dev/null
		ls -d /tmp/dshd-"$prefix".* 2>/dev/null
		pgrep -f "dshd-release-$prefix/" 2>/dev/null
		lsof -n -a -d cwd -F n 2>/dev/null | grep "^n/tmp/dshd-$prefix\." | sed 's/^n/process in /'
	)
	if [ -n "$rest" ]; then
		printf 'FAIL  %s: left behind: %s\n' "$1" "$(printf '%s' "$rest" | tr '\n' ' ')"
		failed=$((failed + 1))
	else
		ok "$1: nothing left behind"
	fi
	if [ -e "$home/.local/state/dshd" ]; then
		printf 'FAIL  %s: the state root, absent before, is still there\n' "$1"
		failed=$((failed + 1))
	fi
}

fresh() {
	chmod -R u+w "$home" 2>/dev/null
	rm -rf "$home"
	mkdir -p "$home/.fake-launchd"
	for marker in "$@"; do
		touch "$home/.fake-launchd/$marker"
	done
}

held_dir() {
	sed -n 's/^  .*acceptance\.sh resume \(.*\)$/\1/p' "$out" | tail -n 1
}

# 1. The whole run, with captures.
fresh
kit_run run "${kit_common[@]}" --captures "$work/captures" --capture-domain
expect "run" "$?" 0 \
	"[0] PASS nothing of prefix $prefix is left from before" \
	"[1] PASS bin/dshd is an arm64 Mach-O" \
	"[1] PASS bin/dshd links only /usr/lib and /System" \
	"[1] PASS libexec/dshd/dshd-run holds no Nix store path" \
	"[1] PASS no nix on dshd's PATH" \
	"[2] PASS dshd release add $prefix" \
	"[2] PASS the entry is read-only" \
	"[2] PASS the entry's bin/dsh is the given DSH, not a copy" \
	"[3] PASS get: state running" \
	"[3] PASS get: unit gui/$(id -u)/dshd.$prefix-web" \
	"[3] PASS get: a web URL" \
	"[3] PASS launchd holds gui/$(id -u)/dshd.$prefix-web" \
	"[3] PASS the job has no schedule, KeepAlive or environment key" \
	"[4] PASS its runner has no controlling terminal" \
	"[5] PASS dshd stop $prefix-web" \
	"[5] PASS launchd no longer holds" \
	"[5] PASS get: lastRun.ending stopped" \
	"[5] PASS get: running under a new invocation" \
	"[6] PASS running again under a new invocation" \
	"[6] PASS the launchd job kept its pid" \
	"[6b] PASS a killed runner is not reported running" \
	"[9] PASS dshd rm $prefix-web" \
	"[9] PASS dshd release rm $prefix" \
	"[9] PASS the release entry is gone" \
	"[9] PASS nothing was left for the sweep" \
	"[9] PASS nothing of the kit remains" \
	"0 failed"
no_fail_rows "run"
left_clean "run"
if grep -r -q -F "$home" "$work/captures" || grep -r -q 'token=stub' "$work/captures"; then
	bad "captures: the home or a web token is in a capture"
elif [ -s "$work/captures/print-running.txt" ] && [ -s "$work/captures/print-ended.txt" ] && [ -s "$work/captures/print-domain.txt" ]; then
	ok "captures: written and scrubbed"
else
	bad "captures: missing files: $(find "$work/captures" -type f -exec basename {} \; | tr '\n' ' ')"
fi

# 2. A hold for a disconnect, then resume.
fresh
kit_run hold "${kit_common[@]}" --for disconnect
status=$?
dir=$(held_dir)
expect "hold disconnect" "$status" 0 "[3] PASS get: state running" "Now close this Terminal window"
if [ -n "$dir" ] && [ -f "$dir/kit/state" ]; then
	kit_run resume "$dir"
	expect "resume disconnect" "$?" 0 \
		"[4] PASS after the session closed: still running" \
		"[4] PASS after the session closed: the same invocation" \
		"[9] PASS nothing of the kit remains"
	no_fail_rows "resume disconnect"
else
	bad "hold disconnect: no DIR to resume"
fi
left_clean "hold and resume disconnect"

# 3. A hold for a logout, a logout and a login, then resume.
fresh
kit_run hold "${kit_common[@]}" --for logout
status=$?
dir=$(held_dir)
expect "hold logout" "$status" 0 "[7] PASS dshd enable $prefix-web" "[7] PASS the copy Library/LaunchAgents/dshd.$prefix-web.plist is written" "Now log out"
env -i HOME="$home" PATH="$sys" bash "$here/fake/dshd" __logout
env -i HOME="$home" PATH="$sys" bash "$here/fake/dshd" __login
if [ -n "$dir" ]; then
	kit_run resume "$dir"
	expect "resume logout" "$?" 0 \
		"[7] PASS after logout and login: running" \
		"[7] PASS after logout and login: a new invocation" \
		"[7] PASS dshd disable $prefix-web" \
		"[7] PASS the LaunchAgents copy is removed" \
		"[9] PASS nothing of the kit remains"
	no_fail_rows "resume logout"
else
	bad "hold logout: no DIR to resume"
fi
left_clean "hold and resume logout"

# 4. No GUI login session: step 8 instead of 3 to 7.
fresh no-gui
kit_run run "${kit_common[@]}"
expect "no GUI session" "$?" 0 \
	"[0] INFO GUI login session" \
	"[8] PASS without a GUI login: exit 7" \
	"[8] PASS the error says to log in" \
	"[3] SKIP steps 3 to 7"
left_clean "no GUI session"

# 5. A job with KeepAlive is a failure, and the clean-up still completes.
fresh keepalive
kit_run run "${kit_common[@]}"
expect "KeepAlive" "$?" 1 \
	"[3] FAIL the job has no schedule, KeepAlive or environment key" \
	"[9] PASS nothing of the kit remains"
left_clean "KeepAlive"

# 6. A stop that leaves DSH running: the sweep reports and removes it.
fresh leaky
kit_run run "${kit_common[@]}"
expect "leaky stop" "$?" 1 \
	"[9] FAIL nothing was left for the sweep" \
	"[9] PASS nothing of the kit remains"
left_clean "leaky stop"

# 7. The skeleton: release unknown, every launchd verb not built.
fresh unbuilt
kit_run run "${kit_common[@]}"
expect "skeleton" "$?" 1 \
	"[1] PASS dshd --version runs" \
	"[2] FAIL dshd release add $prefix" \
	"[3] SKIP steps 3 to 8" \
	"[9] PASS nothing of the kit remains"
left_clean "skeleton"

# 8. A library in /nix/store fails step 1.
fresh nix-dylib
kit_run run "${kit_common[@]}"
expect "nix library" "$?" 1 "[1] FAIL bin/dshd links only /usr/lib and /System"
left_clean "nix library"

# 9. Leftovers refuse a run; clean removes them.
fresh
mkdir -p "$home/.local/state/dshd/$prefix-old"
kit_run run "${kit_common[@]}"
expect "leftovers refuse a run" "$?" 2 "[0] FAIL nothing of prefix $prefix is left from before" "clean --prefix $prefix"
kit_run clean --prefix "$prefix"
expect "clean after leftovers" "$?" 0 "[9] INFO removed by the sweep" "[9] PASS nothing of the kit remains"
rmdir "$home/.local/state/dshd" "$home/.local/state" "$home/.local" 2>/dev/null
left_clean "clean after leftovers"

# 10. A hold given up: clean DIR.
fresh
kit_run hold "${kit_common[@]}" --for disconnect
dir=$(held_dir)
kit_run clean "$dir"
expect "clean a hold" "$?" 0 "[9] PASS dshd rm $prefix-web" "[9] PASS nothing of the kit remains"
left_clean "clean a hold"

printf '\n%s\n' "$([ "$failed" = 0 ] && echo "selftest: all passed" || echo "selftest: $failed failed")"
[ "$failed" = 0 ]
