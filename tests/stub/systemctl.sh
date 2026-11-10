#!/bin/sh
# The fake systemctl of the golden CLI cases; tests/golden/README.md is
# its contract. Everything it knows lives under $FAKE_DIR, whose parent is
# the case's root:
#
#   units/UNIT        the unit's state word: running, dead (ran, then
#                     stopped), failed, masked, or exited N (its headless
#                     task ended by itself with status N); a unit without
#                     a file never ran
#   log/systemctl     one line per call, the arguments space-joined; the
#                     queries show-environment, show and list-units are
#                     not logged
#   manager-down      every call fails as systemctl does without a bus
#   template-missing  dshd.target is not-found
#   unit-path-elsewhere  the manager's UnitPath names a persistent unit
#                     directory the fake reads no links from
#
# FAKE_FAIL=start makes start and restart fail the unit; FAKE_FAIL=stop
# makes stop fail and leaves the state as it was; FAKE_FAIL=daemon-reload
# makes daemon-reload fail. FAKE_EXIT=N makes start
# and restart end the run at once as dsh exiting N would, and record that
# end in the instance's last-run.json as the runner's post-stop step does.

set -u

root=${FAKE_DIR%/*}
units=$FAKE_DIR/units
argv=$*

if [ "${1-}" != --user ]; then
	printf '%s\n' "$argv" >>"$FAKE_DIR/log/systemctl"
	printf 'systemctl: the fake answers --user calls only\n' >&2
	exit 1
fi
shift
verb=${1-}
[ "$#" -gt 0 ] && shift

case $verb in
show-environment | show | list-units) printf '%s\n' "$argv" >>"$FAKE_DIR/log/systemctl-queries" ;;
*) printf '%s\n' "$argv" >>"$FAKE_DIR/log/systemctl" ;;
esac

if [ -e "$FAKE_DIR/manager-down" ]; then
	printf 'Failed to connect to bus: No medium found\n' >&2
	exit 1
fi

state_of() {
	if [ -f "$units/$1" ]; then
		cat "$units/$1"
	else
		printf never
	fi
}

set_state() {
	mkdir -p "$units" && printf '%s\n' "$2" >"$units/$1"
}

# The end the runner's post-stop step records for a task that exited $2.
record_exit() {
	name=${1#dshd@}
	dir=$root/state/dshd/${name%.service}
	[ -d "$dir" ] || return 0
	served=success
	[ "$2" = 0 ] || served=exit-code
	boot=$(cat /proc/sys/kernel/random/boot_id 2>/dev/null) || boot=unknown
	printf '{"id":"","releasePath":"","bootId":"%s","startedAt":1794009610,"invocationId":"0123456789abcdef0123456789abcdef","end":{"endedAt":1794013210,"serviceResult":"%s","exitCode":"exited","exitStatus":"%s","child":{"exitCode":%s},"stopIntent":false,"stopSignal":null}}\n' \
		"$boot" "$served" "$2" "$2" >"$dir/last-run.json"
}

# One show block: every property asked for, valued as the README says.
show_unit() {
	load=loaded active=inactive sub=dead result=success pid=0 status=0
	entered= left= invocation=
	case $1 in
	dshd.target)
		if [ -e "$FAKE_DIR/template-missing" ]; then
			load=not-found
		else
			active=active sub=active
		fi
		;;
	*)
		case $(state_of "$1") in
		running)
			active=active sub=running pid=4242 entered=@1794009610
			invocation=0123456789abcdef0123456789abcdef
			;;
		dead) entered=@1794009610 left=@1794013210 ;;
		failed) active=failed sub=failed result=exit-code status=1 left=@1794013210 ;;
		masked) load=masked ;;
		'exited 0')
			entered=@1794009610 left=@1794013210
			invocation=0123456789abcdef0123456789abcdef
			;;
		exited\ *)
			active=failed sub=failed result=exit-code left=@1794013210
			status=$(state_of "$1")
			status=${status#exited }
			invocation=0123456789abcdef0123456789abcdef
			;;
		esac
		;;
	esac
	saved_ifs=$IFS
	IFS=,
	for property in $properties; do
		case $property in
		Id) value=$1 ;;
		LoadState) value=$load ;;
		ActiveState) value=$active ;;
		SubState) value=$sub ;;
		Result) value=$result ;;
		MainPID) value=$pid ;;
		ExecMainStatus) value=$status ;;
		NRestarts) value=0 ;;
		ActiveEnterTimestamp) value=$entered ;;
		InactiveEnterTimestamp) value=$left ;;
		InvocationID) value=$invocation ;;
		WorkingDirectory)
			value=$root/state/dshd/-
			[ ! -e "$FAKE_DIR/template-missing" ] || value=
			;;
		WantedBy)
			value=
			[ ! -L "$root/config/systemd/user/dshd.target.wants/$1" ] || value=dshd.target
			;;
		CPUUsageNSec) value='[not set]'; [ "$active" != active ] || value=1234567890 ;;
		MemoryCurrent) value='[not set]'; [ "$active" != active ] || value=1048576 ;;
		MemoryPeak) value='[not set]'; [ "$active" != active ] || value=52428800 ;;
		TasksCurrent) value='[not set]'; [ "$active" != active ] || value=12 ;;
		IOReadBytes | IOWriteBytes) value='[not set]' ;;
		*) value= ;;
		esac
		printf '%s=%s\n' "$property" "$value"
	done
	IFS=$saved_ifs
}

case $verb in
show-environment)
	printf 'HOME=%s\nXDG_CONFIG_HOME=%s\nXDG_RUNTIME_DIR=%s\nXDG_STATE_HOME=%s\n' \
		"$root/home" "$root/config" "$root/run" "$root/state"
	;;
show)
	properties=Id,LoadState,ActiveState,SubState,Result,MainPID,ExecMainStatus,NRestarts,ActiveEnterTimestamp,InactiveEnterTimestamp,InvocationID
	shown=0
	while [ "$#" -gt 0 ]; do
		case $1 in
		-p | --property)
			properties=${2-}
			[ "$#" -gt 1 ] && shift
			;;
		--property=*) properties=${1#--property=} ;;
		-*) ;;
		*)
			[ "$shown" = 0 ] || printf '\n'
			shown=1
			show_unit "$1"
			;;
		esac
		shift
	done
	if [ "$shown" = 0 ] && [ "$properties" = UnitPath ]; then
		persistent=$root/config/systemd/user
		[ ! -e "$FAKE_DIR/unit-path-elsewhere" ] || persistent=$root/elsewhere/systemd/user
		printf 'UnitPath=%s.control %s %s %s\n' \
			"$persistent" "$root/run/systemd/user.control" "$persistent" /etc/systemd/user
	fi
	;;
list-units)
	pattern='*'
	for arg in "$@"; do
		case $arg in
		-*) ;;
		*) pattern=$arg ;;
		esac
	done
	for file in "$units"/*; do
		[ -f "$file" ] || continue
		unit=${file##*/}
		# The pattern is a glob on purpose, as systemctl takes it.
		# shellcheck disable=SC2254
		case $unit in
		$pattern) ;;
		*) continue ;;
		esac
		case $(state_of "$unit") in
		running) printf '%s loaded active running dshd instance\n' "$unit" ;;
		failed | exited\ [1-9]*) printf '%s loaded failed failed dshd instance\n' "$unit" ;;
		masked) printf '%s masked inactive dead dshd instance\n' "$unit" ;;
		*) printf '%s loaded inactive dead dshd instance\n' "$unit" ;;
		esac
	done
	;;
start | restart)
	if [ "${FAKE_FAIL:-}" = start ]; then
		set_state "${1-}" failed
		printf 'Job for %s failed because the control process exited with error code.\n' "${1-}" >&2
		exit 1
	fi
	if [ -n "${FAKE_EXIT:-}" ]; then
		set_state "${1-}" "exited $FAKE_EXIT"
		record_exit "${1-}" "$FAKE_EXIT"
		exit 0
	fi
	set_state "${1-}" running
	;;
stop)
	if [ "${FAKE_FAIL:-}" = stop ]; then
		printf 'Failed to stop %s: Access denied\n' "${1-}" >&2
		exit 1
	fi
	set_state "${1-}" dead
	;;
reset-failed)
	case $(state_of "${1-}") in
	failed | exited\ [1-9]*) set_state "${1-}" dead ;;
	esac
	;;
daemon-reload)
	if [ "${FAKE_FAIL:-}" = daemon-reload ]; then
		printf 'Failed to reload daemon: Access denied\n' >&2
		exit 1
	fi
	;;
disable)
	for unit in "$@"; do
		rm -f "$root/config/systemd/user/dshd.target.wants/$unit"
	done
	;;
esac
