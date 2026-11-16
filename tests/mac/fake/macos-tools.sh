#!/usr/bin/env bash
# Stand-ins for the macOS commands tests/mac/acceptance.sh calls, for its
# self-test on Linux (tests/mac/selftest.sh). One script: the name it is
# called by picks the tool. launchctl keeps its jobs in ~/.fake-launchd,
# which tests/mac/fake/dshd shares:
#   dshd.NAME.loaded   the job is bootstrapped
#   dshd.NAME.pid      its runner's pid (the job's pid)
#   no-gui             there is no GUI login session
#   no-clt             the command line tools are not installed
#   nix-dylib          otool lists a library in /nix/store

set -u
launchd=$HOME/.fake-launchd
uid=$(id -u)

marker() {
	[ -e "$launchd/$1" ]
}

launchctl_print() {
	local target=$1 label pid
	if marker no-gui; then
		printf 'Bad request.\nCould not find domain for user gui: %s\n' "$uid" >&2
		exit 125
	fi
	case $target in
	gui/*/*)
		label=${target#gui/*/}
		if [ ! -e "$launchd/$label.loaded" ]; then
			printf 'Bad request.\nCould not find service "%s" in domain for user gui: %s\n' "$label" "$uid" >&2
			exit 113
		fi
		pid=$(cat "$launchd/$label.pid" 2>/dev/null)
		printf '%s = {\n\tactive count = 1\n\tpath = %s/Library/LaunchAgents/%s.plist\n' "$target" "$HOME" "$label"
		if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
			printf '\tstate = running\n\tpid = %s\n' "$pid"
		else
			printf '\tstate = not running\n\tlast exit code = (never exited)\n'
		fi
		printf '}\n'
		;;
	gui/*)
		printf '%s = {\n\ttype = login\n\thandle = %s\n\tactive count = 1\n}\n' "$target" "$uid"
		;;
	*)
		printf 'Unrecognized target specifier.\n' >&2
		exit 1
		;;
	esac
}

launchctl_list() {
	local file label pid
	printf 'PID\tStatus\tLabel\n'
	for file in "$launchd"/*.loaded; do
		[ -e "$file" ] || continue
		label=$(basename "$file" .loaded)
		pid=$(cat "$launchd/$label.pid" 2>/dev/null)
		printf '%s\t0\t%s\n' "${pid:--}" "$label"
	done
}

launchctl_bootout() {
	local label=${1#gui/*/} pid
	if [ ! -e "$launchd/$label.loaded" ]; then
		printf 'Boot-out failed: 3: No such process\n' >&2
		exit 3
	fi
	pid=$(cat "$launchd/$label.pid" 2>/dev/null)
	if [ -n "$pid" ]; then
		kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null
	fi
	rm -f "$launchd/$label.loaded" "$launchd/$label.pid"
}

case $(basename "$0") in
uname)
	case ${1-} in
	-m) echo arm64 ;;
	*) echo Darwin ;;
	esac
	;;
sw_vers)
	if [ "${1-}" = -productVersion ]; then
		echo 26.0
	else
		printf 'ProductName:\tmacOS\nProductVersion:\t26.0\n'
	fi
	;;
hostname) echo testhost ;;
xcode-select)
	if marker no-clt; then
		echo "xcode-select: error: unable to get active developer directory" >&2
		exit 2
	fi
	echo /Library/Developer/CommandLineTools
	;;
otool)
	printf '%s:\n\t/usr/lib/libSystem.B.dylib (compatibility version 1.0.0, current version 1351.0.0)\n' "${2-}"
	if marker nix-dylib; then
		printf '\t/nix/store/00000000000000000000000000000000-libiconv/lib/libiconv.2.dylib (compatibility version 7.0.0)\n'
	fi
	;;
file) echo "Mach-O 64-bit executable arm64" ;;
codesign)
	case ${1-} in
	--display) echo "Signature=adhoc" >&2 ;;
	esac
	;;
xattr) exit 1 ;;
plutil) exit 0 ;;
launchctl)
	case ${1-} in
	print) launchctl_print "${2-}" ;;
	print-disabled) printf 'disabled services = {\n}\n' ;;
	list) launchctl_list ;;
	bootout) launchctl_bootout "${2-}" ;;
	*)
		printf 'fake launchctl: %s is not faked\n' "${1-}" >&2
		exit 64
		;;
	esac
	;;
*)
	printf 'macos-tools.sh: not a faked tool: %s\n' "$(basename "$0")" >&2
	exit 64
	;;
esac
