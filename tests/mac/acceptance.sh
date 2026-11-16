#!/bin/bash
# The acceptance of dshd on a Mac without Nix: the steps of the Mac test
# plan, run under one temporary prefix and a throwaway instance prefix, then
# a complete clean-up, ending with a plain pass/fail table.
#
#   acceptance.sh run    --dshd FILE --dsh FILE [OPTIONS]
#   acceptance.sh hold   --dshd FILE --dsh FILE --for disconnect|logout [OPTIONS]
#   acceptance.sh resume DIR
#   acceptance.sh clean  [--prefix WORD] [DIR]
#
# run     every step one session can check, then the clean-up.
# hold    the steps up to a running web instance (for logout, also enabled),
#         then it stops and says what to do by hand: close the Terminal
#         window or SSH session, or log out at the screen and in again.
#         Then resume.
# resume  checks what the hold asked for, then the remaining steps and the
#         clean-up. DIR is the temporary prefix the hold printed.
# clean   removes what a run or hold of the kit left, whatever its state:
#         after an interrupted run, or a hold that will not be resumed.
#
# Options:
#   --dshd FILE        the dshd binary under test (arm64 Mach-O)
#   --dshd-run FILE    its runner; default: ../libexec/dshd/dshd-run from the
#                      directory of --dshd, else dshd-run beside it
#   --dsh FILE         a stock DSH, or tests/stub/dsh.sh; used where it is,
#                      never copied or changed
#   --prefix WORD      the instance prefix, 1 to 8 letters a-z; default zzt
#   --path DIR         a directory DSH needs on PATH (its interpreter),
#                      put after the system directories
#   --own-dsh-home     let DSH use its own home; by default DSH_HOME is a
#                      directory inside the temporary prefix
#   --captures DIR     keep launchctl print text and dshd get JSON there,
#                      with the user name, home, host name and web token
#                      replaced; it also captures an ended and a killed job
#   --capture-domain   with --captures, also keep print gui/UID and
#                      print-disabled gui/UID, which name every job of the
#                      user: review them before they go anywhere
#   --timeout SECONDS  how long one wait lasts; default 60
#
# Everything the kit makes lives in /tmp/dshd-PREFIX.XXXXXX (the binaries,
# DSH_HOME, the working directory), except what dshd itself writes:
# ~/.local/state/dshd/PREFIX-*, ~/.local/state/dshd/.releases, the release
# name, and during a logout hold ~/Library/LaunchAgents/dshd.PREFIX-web.plist.
# The clean-up removes them through dshd, then sweeps what dshd left and
# reports it as a failure, and removes the state root when it did not
# exist before. dshd runs with PATH limited to the prefix's bin and the
# system directories, and with no environment but the login's basics.
#
# launchctl: the kit uses print and list, and bootout of its own labels in
# the sweep only; never kickstart, load, enable or disable.
#
# Exit: 0 when no check failed, 1 when one did, 2 for usage or a refused
# environment.
#
# For the kit's own test on Linux (tests/mac/selftest.sh) only:
# DSHD_KIT_HOME replaces the home from the user database, and
# DSHD_KIT_SYSTEM_PATH the system directories.

set -u
set -o pipefail

system_path=${DSHD_KIT_SYSTEM_PATH:-/usr/bin:/bin:/usr/sbin:/sbin}
PATH=$system_path
export PATH

tab=$(printf '\t')
mode=
dshd_in=
runner_in=
dsh_in=
prefix=zzt
extra_path=
own_dsh_home=0
captures=
capture_domain=0
timeout=60
hold_for=
T=
kit=
names=
release_added=0
invocation=
mainpid=
state_root_existed=1
releases_existed=1
agents_existed=1
gui=0
failures=0
last_timed_out=0
last_status=0
cleaning=0

usage() {
	sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//' >&2
	exit 2
}

die() {
	printf 'acceptance.sh: %s\n' "$*" >&2
	exit 2
}

# One row of the table: STEP RESULT CHECK [DETAIL]. RESULT is PASS, FAIL,
# SKIP or INFO.
rows=$(mktemp /tmp/dshd-kit-rows.XXXXXX) || die "mktemp failed"
done_or_held=0
trap on_exit EXIT
record() {
	local detail=${4-}
	detail=$(printf '%s' "$detail" | tr '\t\n' '  ' | cut -c 1-180)
	printf '%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$detail" >>"$rows"
	printf '[%s] %-4s %s%s\n' "$1" "$2" "$3" "${detail:+ ($detail)}" >&2
	[ "$2" != FAIL ] || failures=$((failures + 1))
}

check() {
	if [ "$4" = 0 ]; then record "$1" PASS "$2" "$3"; else record "$1" FAIL "$2" "$3"; fi
}

print_table() {
	local step result what detail pass=0 fail=0 skip=0 info=0
	printf '\n%-5s %-6s %-52s %s\n' STEP RESULT CHECK DETAIL
	while IFS="$tab" read -r step result what detail; do
		printf '%-5s %-6s %-52s %s\n' "$step" "$result" "$what" "$detail"
		case $result in
		PASS) pass=$((pass + 1)) ;;
		FAIL) fail=$((fail + 1)) ;;
		SKIP) skip=$((skip + 1)) ;;
		*) info=$((info + 1)) ;;
		esac
	done <"$rows"
	printf '\n%s passed, %s failed, %s skipped, %s facts\n' "$pass" "$fail" "$skip" "$info"
}

# --- small helpers -------------------------------------------------------

first_line() {
	head -n 1 "$1" 2>/dev/null | cut -c 1-160
}

# The canonical path of FILE, following every symbolic link.
canonical() {
	local path=$1 link dir n=0
	while [ -L "$path" ] && [ "$n" -lt 40 ]; do
		link=$(readlink "$path") || return 1
		case $link in
		/*) path=$link ;;
		*) path=$(dirname "$path")/$link ;;
		esac
		n=$((n + 1))
	done
	dir=$(cd -P "$(dirname "$path")" 2>/dev/null && pwd) || return 1
	printf '%s/%s\n' "$dir" "$(basename "$path")"
}

# Each argument that exists, one per line.
existing() {
	local path
	for path in "$@"; do
		if [ -e "$path" ] || [ -L "$path" ]; then
			printf '%s\n' "$path"
		fi
	done
}

# KEYPATH (dot separated) of the JSON in FILE, as text; empty for null or
# missing. jq where the system has it, else JavaScript for Automation.
json_get() {
	if command -v jq >/dev/null 2>&1; then
		jq -r --arg p "$2" 'try (getpath($p | split(".")) | if . == null then "" else tostring end) catch ""' "$1" 2>/dev/null
	else
		osascript -l JavaScript "$kit/json.js" "$1" "$2" 2>/dev/null
	fi
}

write_json_reader() {
	cat >"$kit/json.js" <<'EOF'
function run(argv) {
  var text = $.NSString.stringWithContentsOfFileEncodingError(argv[0], $.NSUTF8StringEncoding, null);
  if (!text) return '';
  var value;
  try { value = JSON.parse(text.js); } catch (e) { return ''; }
  argv[1].split('.').forEach(function (key) {
    value = (value === null || value === undefined) ? undefined : value[key];
  });
  if (value === null || value === undefined) return '';
  return typeof value === 'object' ? JSON.stringify(value) : String(value);
}
EOF
}

regex_quote() {
	printf '%s' "$1" | sed 's/[][\.*^$|/+?(){}]/\\&/g'
}

# Copies stdin to FILE with the home, user name and host name replaced and
# web tokens removed.
scrub_to() {
	sed -e "s|$(regex_quote "$home")|/Users/user|g" \
		-e "s|$(regex_quote "$user")|user|g" \
		-e "s|$(regex_quote "$host")|host|g" \
		-e 's|token=[A-Za-z0-9._~-]*|token=redacted|g' >"$1"
}

capture() {
	[ -n "$captures" ] || return 0
	local file=$1
	shift
	{
		"$@" 2>&1
		printf 'exit %s\n' "$?"
	} | scrub_to "$captures/$file"
}

capture_file() {
	[ -n "$captures" ] || return 0
	scrub_to "$captures/$2" <"$1"
}

# --- dshd under the kit's environment -------------------------------------

set_environment() {
	kit_path="$T/bin:$system_path${extra_path:+:$extra_path}"
	kit_env=("HOME=$home" "USER=$user" "LOGNAME=$user" "SHELL=${SHELL:-/bin/sh}" "LANG=${LANG:-en_US.UTF-8}" "TMPDIR=${TMPDIR:-/tmp}" "PATH=$kit_path")
	if [ "$own_dsh_home" = 0 ]; then
		kit_env+=("DSH_HOME=$T/dsh-home")
	fi
}

# call_dshd SECONDS ARGS...: dshd's stdout in $kit/out, stderr in $kit/err,
# its exit status in last_status and returned; 255 with last_timed_out=1 when it outlasted
# SECONDS and was killed.
call_dshd() {
	local limit=$(($1 * 5)) waited=0 pid
	shift
	last_timed_out=0
	env -i "${kit_env[@]}" "$T/bin/dshd" "$@" >"$kit/out" 2>"$kit/err" </dev/null &
	pid=$!
	while kill -0 "$pid" 2>/dev/null; do
		if [ "$waited" -ge "$limit" ]; then
			kill -TERM "$pid" 2>/dev/null
			sleep 1
			kill -KILL "$pid" 2>/dev/null
			wait "$pid" 2>/dev/null
			last_timed_out=1
			last_status=255
			return 255
		fi
		sleep 0.2
		waited=$((waited + 1))
	done
	wait "$pid"
	last_status=$?
	return "$last_status"
}

# What went wrong in the last call: the error kind and message, or the
# first line of stderr.
last_error() {
	local kind message
	kind=$(json_get "$kit/err" error.kind)
	message=$(json_get "$kit/err" error.message)
	if [ "$last_timed_out" = 1 ]; then
		printf 'timed out'
	elif [ -n "$kind" ]; then
		printf '%s: %s' "$kind" "$message" | cut -c 1-160
	else
		first_line "$kit/err"
	fi
}

# get NAME: dshd get NAME --json into $kit/get.json; its status.
get() {
	call_dshd "$timeout" get "$1" --json
	local status=$?
	cp "$kit/out" "$kit/get.json"
	return "$status"
}

field() {
	json_get "$kit/get.json" "$1"
}

# wait_for SECONDS COMMAND...: retries COMMAND every second until it
# succeeds; status 1 when SECONDS pass first.
wait_for() {
	local deadline=$(($(date +%s) + $1))
	shift
	while ! "$@"; do
		[ "$(date +%s)" -lt "$deadline" ] || return 1
		sleep 1
	done
}

job() {
	printf 'gui/%s/dshd.%s' "$uid" "$1"
}

job_loaded() {
	launchctl print "$(job "$1")" >/dev/null 2>&1
}

job_pid() {
	launchctl print "$(job "$1")" 2>/dev/null | sed -n 's/^[[:space:]]*pid = \([0-9][0-9]*\)$/\1/p' | head -n 1
}

# --- state of a held run ---------------------------------------------------

save_state() {
	cat >"$kit/state" <<EOF
prefix=$prefix
hold_for=$hold_for
names=$names
release_added=$release_added
invocation=$invocation
mainpid=$mainpid
state_root_existed=$state_root_existed
releases_existed=$releases_existed
agents_existed=$agents_existed
own_dsh_home=$own_dsh_home
extra_path=$extra_path
captures=$captures
capture_domain=$capture_domain
timeout=$timeout
EOF
}

load_state() {
	local key value
	[ -f "$1/kit/state" ] || die "$1 is not a temporary prefix of this kit (no kit/state)"
	T=$(cd -P "$1" && pwd)
	kit=$T/kit
	while IFS='=' read -r key value; do
		case $key in
		prefix) prefix=$value ;;
		hold_for) hold_for=$value ;;
		names) names=$value ;;
		release_added) release_added=$value ;;
		invocation) invocation=$value ;;
		mainpid) mainpid=$value ;;
		state_root_existed) state_root_existed=$value ;;
		releases_existed) releases_existed=$value ;;
		agents_existed) agents_existed=$value ;;
		own_dsh_home) own_dsh_home=$value ;;
		extra_path) extra_path=$value ;;
		captures) captures=$value ;;
		capture_domain) capture_domain=$value ;;
		timeout) timeout=$value ;;
		esac
	done <"$kit/state"
}

add_name() {
	case " $names " in
	*" $1 "*) ;;
	*) names=${names:+$names }$1 ;;
	esac
	save_state
}

# --- what the kit's prefix holds outside the temporary prefix ---------------

# One line per thing of the prefix that exists outside the temporary
# prefix, and per process running from it.
leftovers() {
	local name
	existing "$state_root/$prefix"-* "$agents/dshd.$prefix"-*.plist "$state_root/.releases/"*"-dshd-release-$prefix"
	launchctl list 2>/dev/null | awk -v p="dshd.$prefix-" 'index($3, p) == 1 { print "launchd job " $3 }'
	for name in $names; do
		if job_loaded "$name"; then
			printf 'launchd job %s\n' "$(job "$name")"
		fi
	done | sort -u
	leftover_processes | sed 's/^/process /'
}

# Processes that run a file of the temporary prefix or a release entry of
# the prefix, or whose working directory is in the temporary prefix; never
# the kit itself.
leftover_processes() {
	[ -n "$T" ] || return 0
	{
		# pgrep -f cannot leave out the kit's own subshells, whose command
		# line names the temporary prefix in resume and clean.
		# shellcheck disable=SC2009
		ps -A -o pid= -o command= 2>/dev/null | grep -F -e "$T/" -e "-dshd-release-$prefix/" | grep -v -F -e acceptance.sh -e 'grep -F' | awk '{ print $1 }'
		if command -v lsof >/dev/null 2>&1; then
			lsof -n -a -d cwd -F n 2>/dev/null | awk -v t="$T/" '/^p/ { pid = substr($0, 2) } /^n/ && index(substr($0, 2) "/", t) == 1 { print pid }'
		fi
	} | grep -v -x -e "$$" | sort -u
}

# --- steps -------------------------------------------------------------------

preflight() {
	local os arch product
	os=$(uname -s)
	arch=$(uname -m)
	if [ "$os" != Darwin ] || [ "$arch" != arm64 ]; then
		record 0 FAIL "macOS on Apple silicon" "$os $arch"
		print_table
		exit 2
	fi
	product=$(sw_vers -productVersion 2>/dev/null)
	record 0 INFO "macOS on Apple silicon" "macOS $product $arch"
	[ "$(id -u)" != 0 ] || die "run as the login user, not as root"
	if launchctl print "gui/$uid" >/dev/null 2>&1; then
		gui=1
		record 0 INFO "GUI login session" "gui/$uid present"
	else
		gui=0
		record 0 INFO "GUI login session" "gui/$uid absent: step 8 runs, steps 3 to 7 do not"
	fi
	if command -v jq >/dev/null 2>&1; then
		record 0 INFO "JSON reader" "$(command -v jq)"
	elif command -v osascript >/dev/null 2>&1; then
		record 0 INFO "JSON reader" "osascript (JavaScript)"
	else
		die "no JSON reader: neither jq nor osascript is on $system_path"
	fi
	if [ -n "$(leftovers)" ]; then
		record 0 FAIL "nothing of prefix $prefix is left from before" "$(leftovers | tr '\n' ' ')"
		printf '\nRun: %s clean --prefix %s\n' "$0" "$prefix" >&2
		print_table
		exit 2
	fi
	record 0 PASS "nothing of prefix $prefix is left from before"
	[ -d "$state_root" ] || state_root_existed=0
	[ -d "$state_root/.releases" ] || releases_existed=0
	[ -d "$agents" ] || agents_existed=0
}

setup() {
	local dsh_path
	T=$(mktemp -d "/tmp/dshd-$prefix.XXXXXX") || die "mktemp failed"
	T=$(cd -P "$T" && pwd)
	kit=$T/kit
	mkdir -p "$T/bin" "$T/libexec/dshd" "$kit" "$T/work" "$T/dsh-home" || die "cannot lay out $T"
	save_state
	if ! cp "$dshd_in" "$T/bin/dshd" || ! chmod 0755 "$T/bin/dshd"; then
		die "cannot copy $dshd_in"
	fi
	if ! cp "$runner_in" "$T/libexec/dshd/dshd-run" || ! chmod 0755 "$T/libexec/dshd/dshd-run"; then
		die "cannot copy $runner_in"
	fi
	dsh_path=$(canonical "$dsh_in") || die "cannot resolve $dsh_in"
	ln -s "$dsh_path" "$T/bin/dsh" || die "cannot link the DSH"
	write_json_reader
	set_environment
	if [ -n "$captures" ]; then
		mkdir -p "$captures" || die "cannot make $captures"
	fi
	record 0 INFO "temporary prefix" "$T"
}

# How to list a binary's libraries, the first way this Mac has: otool when
# the command line tools are installed (the /usr/bin/otool shim would
# otherwise ask to install them), dyld_info, else dyld's own report of what
# it loads for a run of --version.
library_lister() {
	if xcode-select -p >/dev/null 2>&1 && command -v otool >/dev/null 2>&1; then
		echo otool
	elif command -v dyld_info >/dev/null 2>&1; then
		echo dyld_info
	else
		echo DYLD_PRINT_LIBRARIES
	fi
}

# linked_libraries LISTER FILE: the libraries FILE links, one per line.
linked_libraries() {
	case $1 in
	otool) otool -L "$2" | tail -n +2 | awk '{ print $1 }' ;;
	dyld_info) dyld_info -dependents "$2" | awk '/^[[:space:]]/ && $NF ~ /^\// { print $NF }' ;;
	*) env -i DYLD_PRINT_LIBRARIES=1 "$2" --version 2>&1 >/dev/null | sed -n 's/^dyld\[[0-9]*\]: <[^>]*> //p' | grep -v -x -F "$2" ;;
	esac
}

step_binaries() {
	local file label description libraries outside signature version libraries_by
	libraries_by=$(library_lister)
	for file in "$T/bin/dshd" "$T/libexec/dshd/dshd-run"; do
		label=${file#"$T"/}
		description=$(file -b "$file")
		case $description in
		*Mach-O*64-bit*arm64*) record 1 PASS "$label is an arm64 Mach-O" ;;
		*) record 1 FAIL "$label is an arm64 Mach-O" "$description" ;;
		esac
		if codesign --verify "$file" >/dev/null 2>&1; then
			signature=$(codesign --display --verbose=2 "$file" 2>&1 | sed -n 's/^Signature=//p; s/^Authority=//p' | head -n 1)
			record 1 PASS "$label carries a valid signature" "${signature:-ad hoc}"
		else
			record 1 FAIL "$label carries a valid signature" "arm64 macOS refuses to run an unsigned binary"
		fi
		if xattr -p com.apple.quarantine "$file" >/dev/null 2>&1; then
			record 1 INFO "$label is quarantined" "Gatekeeper may refuse it; the copy came through a browser"
		fi
		libraries=$(linked_libraries "$libraries_by" "$file")
		outside=$(printf '%s\n' "$libraries" | grep -v -E '^(/usr/lib/|/System/|$)')
		if [ -z "$libraries" ]; then
			record 1 FAIL "$label links only /usr/lib and /System" "no library list ($libraries_by)"
		elif [ -n "$outside" ]; then
			record 1 FAIL "$label links only /usr/lib and /System" "$(printf '%s' "$outside" | tr '\n' ' ')"
		else
			record 1 PASS "$label links only /usr/lib and /System" "$(printf '%s\n' "$libraries" | wc -l | tr -d ' ') libraries, by $libraries_by"
		fi
		# dshd itself knows the string /nix/store; a Nix-built binary holds
		# a store path with its hash.
		if grep -a -q -E '/nix/store/[0-9a-z]{32}-' "$file"; then
			record 1 FAIL "$label holds no Nix store path"
		else
			record 1 PASS "$label holds no Nix store path"
		fi
	done
	if version=$(env -i "${kit_env[@]}" "$T/bin/dshd" --version 2>&1); then
		record 1 PASS "dshd --version runs" "$version"
	else
		record 1 FAIL "dshd --version runs" "$version"
	fi
	if env -i "PATH=$kit_path" /bin/sh -c 'command -v nix' >/dev/null 2>&1; then
		record 1 FAIL "no nix on dshd's PATH" "$kit_path"
	else
		case ":$kit_path:" in
		*:/nix/*) record 1 FAIL "no nix on dshd's PATH" "$kit_path" ;;
		*) record 1 PASS "no nix on dshd's PATH" "$kit_path" ;;
		esac
	fi
	case $(canonical "$T/bin/dsh") in
	/nix/*) record 1 FAIL "DSH does not come from Nix" "$(canonical "$T/bin/dsh")" ;;
	*) record 1 PASS "DSH does not come from Nix" "$(canonical "$T/bin/dsh")" ;;
	esac
}

step_release() {
	local entries count entry path writable=
	if ! call_dshd "$timeout" release add "$prefix" --dsh "$T/bin/dsh"; then
		record 2 FAIL "dshd release add $prefix --dsh \$T/bin/dsh" "$(last_error)"
		return 1
	fi
	release_added=1
	save_state
	record 2 PASS "dshd release add $prefix --dsh \$T/bin/dsh"
	entries=$(existing "$state_root/.releases/"*"-dshd-release-$prefix")
	count=$(printf '%s' "$entries" | grep -c .)
	if [ "$count" != 1 ]; then
		record 2 FAIL "one entry under ~/.local/state/dshd/.releases" "$count found"
		return 0
	fi
	entry=$entries
	record 2 PASS "one entry under ~/.local/state/dshd/.releases" "$(basename "$entry")"
	for path in "$entry" "$entry/bin" "$entry/bin/dshd-run" "$entry/release.json"; do
		if [ ! -e "$path" ]; then
			writable="$writable ${path#"$entry"/}:missing"
		elif [ -w "$path" ]; then
			writable="$writable ${path#"$entry"/}:writable"
		fi
	done
	check 2 "the entry is read-only" "$writable" "$([ -z "$writable" ] && echo 0)"
	if [ "$(canonical "$entry/bin/dsh")" = "$(canonical "$T/bin/dsh")" ]; then
		record 2 PASS "the entry's bin/dsh is the given DSH, not a copy"
	else
		record 2 FAIL "the entry's bin/dsh is the given DSH, not a copy" "$(canonical "$entry/bin/dsh")"
	fi
}

step_web() {
	local name=$prefix-web state unit url plist keys key bad=
	add_name "$name"
	if ! call_dshd $((timeout + 160)) run --name "$name" --release "$prefix" --cwd "$T/work" --json -- web; then
		record 3 FAIL "dshd run --name $name --release $prefix -- web" "$(last_error)"
		return 1
	fi
	record 3 PASS "dshd run --name $name --release $prefix -- web" "$(json_get "$kit/out" outcome)"
	get "$name"
	state=$(field state)
	unit=$(field unit)
	url=$(field web.url)
	invocation=$(field invocationId)
	mainpid=$(field mainPid)
	save_state
	capture_file "$kit/get.json" get-running.json
	check 3 "get: state running" "$state" "$([ "$state" = running ] && echo 0)"
	check 3 "get: unit $(job "$name")" "$unit" "$([ "$unit" = "$(job "$name")" ] && echo 0)"
	check 3 "get: a web URL" "${url%%\?*}" "$([ -n "$url" ] && echo 0)"
	check 3 "get: an invocation id" "$invocation" "$([ -n "$invocation" ] && echo 0)"
	record 3 INFO "get: linger" "$(field linger)"
	if job_loaded "$name"; then
		record 3 PASS "launchd holds $(job "$name")" "pid $(job_pid "$name")"
	else
		record 3 FAIL "launchd holds $(job "$name")"
	fi
	capture print-running.txt launchctl print "$(job "$name")"
	plist=$state_root/$name/launchd.plist
	if [ -f "$plist" ]; then
		keys=$(grep -o '<key>[^<]*</key>' "$plist" | sed 's/<[^>]*>//g' | tr '\n' ' ')
		for key in KeepAlive StartInterval StartCalendarInterval WatchPaths QueueDirectories StartOnMount Sockets Disabled EnvironmentVariables; do
			case " $keys " in *" $key "*) bad="$bad $key" ;; esac
		done
		check 3 "the job has no schedule, KeepAlive or environment key" "${bad:-keys: $keys}" "$([ -z "$bad" ] && echo 0)"
		capture_file "$plist" launchd.plist
	else
		record 3 FAIL "the job has no schedule, KeepAlive or environment key" "no $plist"
	fi
	if [ -e "$agents/dshd.$name.plist" ]; then
		record 3 FAIL "not enabled: no copy in ~/Library/LaunchAgents"
	else
		record 3 PASS "not enabled: no copy in ~/Library/LaunchAgents"
	fi
	[ "$state" = running ]
}

step_detached() {
	local parent tty
	if [ -z "$mainpid" ]; then
		record 4 FAIL "the instance is detached from this session" "no mainPid"
		return
	fi
	parent=$(ps -o ppid= -p "$mainpid" | tr -d ' ')
	tty=$(ps -o tty= -p "$mainpid" | tr -d ' ')
	check 4 "its runner's parent is launchd" "pid $mainpid, parent $parent" "$([ "$parent" = 1 ] && echo 0)"
	case $tty in
	'??' | '?' | '') record 4 PASS "its runner has no controlling terminal" ;;
	*) record 4 FAIL "its runner has no controlling terminal" "$tty" ;;
	esac
}

step_enable() {
	local name=$prefix-web copy=$agents/dshd.$prefix-web.plist
	call_dshd "$timeout" enable "$name" --json
	check 7 "dshd enable $name" "$(last_error)" "$last_status"
	if [ -f "$copy" ]; then
		record 7 PASS "the copy Library/LaunchAgents/dshd.$name.plist is written"
		if command -v plutil >/dev/null 2>&1; then
			plutil -lint "$copy" >/dev/null 2>&1
			check 7 "the LaunchAgents copy is a valid plist" "" "$?"
		fi
	else
		record 7 FAIL "the copy Library/LaunchAgents/dshd.$name.plist is written"
	fi
}

step_disable() {
	local name=$prefix-web
	call_dshd "$timeout" disable "$name" --json
	check 7 "dshd disable $name" "$(last_error)" "$last_status"
	if [ -e "$agents/dshd.$name.plist" ]; then
		record 7 FAIL "the LaunchAgents copy is removed"
	else
		record 7 PASS "the LaunchAgents copy is removed"
	fi
}

is_state() {
	get "$1" && [ "$(field state)" = "$2" ]
}

job_gone() {
	! job_loaded "$1"
}

step_stop_start() {
	local name=$prefix-web before state ending
	before=$invocation
	call_dshd $((timeout + 40)) stop "$name" --json
	check 5 "dshd stop $name" "$(last_error)" "$last_status"
	if wait_for 5 job_gone "$name"; then
		record 5 PASS "launchd no longer holds $(job "$name")"
	else
		record 5 FAIL "launchd no longer holds $(job "$name")"
	fi
	capture print-after-stop.txt launchctl print "$(job "$name")"
	get "$name"
	state=$(field state)
	ending=$(field lastRun.ending)
	check 5 "get: state stopped" "$state" "$([ "$state" = stopped ] && echo 0)"
	check 5 "get: lastRun.ending stopped" "$ending" "$([ "$ending" = stopped ] && echo 0)"
	capture_file "$kit/get.json" get-stopped.json
	call_dshd $((timeout + 160)) start "$name" --json
	check 5 "dshd start $name" "$(last_error)" "$last_status"
	get "$name"
	state=$(field state)
	invocation=$(field invocationId)
	mainpid=$(field mainPid)
	save_state
	check 5 "get: running under a new invocation" "$state, $invocation" "$([ "$state" = running ] && [ -n "$invocation" ] && [ "$invocation" != "$before" ] && echo 0)"
}

new_run() {
	get "$1" && [ "$(field state)" = running ] && [ -n "$(field invocationId)" ] && [ "$(field invocationId)" != "$2" ]
}

step_crash() {
	local name=$prefix-web runner child before started elapsed
	before=$invocation
	runner=$(job_pid "$name")
	runner=${runner:-$mainpid}
	child=$(pgrep -P "$runner" 2>/dev/null | head -n 1)
	if [ -z "$child" ]; then
		record 6 FAIL "DSH runs as a child of the runner" "no child of pid $runner"
		return
	fi
	record 6 INFO "DSH killed with SIGKILL" "pid $child, child of the runner $runner"
	started=$(date +%s)
	kill -KILL "$child"
	if wait_for $((timeout > 30 ? timeout : 30)) new_run "$name" "$before"; then
		elapsed=$(($(date +%s) - started))
		record 6 PASS "running again under a new invocation" "after ${elapsed}s"
	else
		record 6 FAIL "running again under a new invocation" "state $(field state), invocation $(field invocationId)"
	fi
	invocation=$(field invocationId)
	mainpid=$(field mainPid)
	save_state
	check 6 "the launchd job kept its pid" "before $runner, now $(job_pid "$name")" "$([ "$(job_pid "$name")" = "$runner" ] && echo 0)"
	record 6 INFO "get: restarts" "$(field restarts)"
	capture print-restarted.txt launchctl print "$(job "$name")"
	capture_file "$kit/get.json" get-restarted.json
}

not_running() {
	get "$1" && [ "$(field state)" != running ]
}

step_killed_runner() {
	local name=$prefix-web runner child
	runner=$(job_pid "$name")
	if [ -z "$runner" ]; then
		record 6b SKIP "a killed runner is not reported running" "no runner pid"
		return
	fi
	child=$(pgrep -P "$runner" 2>/dev/null | head -n 1)
	kill -KILL "$runner"
	if wait_for "$timeout" not_running "$name"; then
		record 6b PASS "a killed runner is not reported running" "state $(field state), ending $(field lastRun.ending)"
	else
		record 6b FAIL "a killed runner is not reported running" "state $(field state)"
	fi
	if [ -n "$child" ]; then
		sleep 2
		if kill -0 "$child" 2>/dev/null; then
			record 6b INFO "its DSH outlived the runner" "pid $child"
		else
			record 6b INFO "its DSH ended with the runner" "pid $child"
		fi
	fi
	capture print-killed.txt launchctl print "$(job "$name")"
	capture_file "$kit/get.json" get-killed.json
}

step_ended_capture() {
	local name=$prefix-end
	[ -n "$captures" ] || return 0
	add_name "$name"
	call_dshd $((timeout + 160)) run --bg --name "$name" --release "$prefix" --cwd "$T/work" --json -- headless --version
	record 10 INFO "an ended job: dshd run --bg --name $name -- headless --version" "exit $?"
	wait_for "$timeout" not_running "$name"
	record 10 INFO "the ended job's state" "$(field state), ending $(field lastRun.ending)"
	capture print-ended.txt launchctl print "$(job "$name")"
	capture_file "$kit/get.json" get-ended.json
}

step_domain_capture() {
	[ -n "$captures" ] && [ "$capture_domain" = 1 ] || return 0
	capture print-domain.txt launchctl print "gui/$uid"
	capture print-disabled.txt launchctl print-disabled "gui/$uid"
	record 10 INFO "domain text captured" "it names every job of this user: review it"
}

step_no_gui() {
	local name=$prefix-web
	add_name "$name"
	call_dshd "$timeout" run --name "$name" --release "$prefix" --cwd "$T/work" --json -- web
	local status=$?
	check 8 "without a GUI login: exit 7" "exit $status, $(last_error)" "$([ "$status" = 7 ] && echo 0)"
	if grep -q -i 'log in' "$kit/err"; then
		record 8 PASS "the error says to log in"
	else
		record 8 FAIL "the error says to log in" "$(first_line "$kit/err")"
	fi
}

# --- clean-up ----------------------------------------------------------------

entries_of() {
	find "$1" -mindepth 1 -maxdepth 1 -exec basename {} \; | tr '\n' ' '
}

remove_tree() {
	case $1 in
	/tmp/dshd-"$prefix".* | /private/tmp/dshd-"$prefix".* | "$state_root"/* | "$agents"/dshd."$prefix"-*) ;;
	*)
		printf 'acceptance.sh: refusing to remove %s\n' "$1" >&2
		return 1
		;;
	esac
	chmod -R u+w "$1" 2>/dev/null
	rm -rf "$1"
}

# Removes what dshd left of the prefix: jobs, LaunchAgents copies, instance
# directories, release entries and processes. Prints what it removed.
sweep() {
	local item pid label
	for pid in $(leftover_processes); do
		printf 'process %s\n' "$pid"
		kill -TERM "$pid" 2>/dev/null
	done
	sleep 1
	for pid in $(leftover_processes); do
		kill -KILL "$pid" 2>/dev/null
	done
	launchctl list 2>/dev/null | awk -v p="dshd.$prefix-" 'index($3, p) == 1 { print $3 }' >"$rows.labels"
	for item in $names; do
		printf 'dshd.%s\n' "$item"
	done >>"$rows.labels"
	sort -u "$rows.labels" | while IFS= read -r label; do
		if launchctl print "gui/$uid/$label" >/dev/null 2>&1; then
			printf 'launchd job %s\n' "$label"
			launchctl bootout "gui/$uid/$label" >/dev/null 2>&1
		fi
	done
	rm -f "$rows.labels"
	for item in $(existing "$agents/dshd.$prefix"-*.plist "$state_root/$prefix"-* "$state_root/.releases/"*"-dshd-release-$prefix"); do
		printf '%s\n' "${item#"$home"/}"
		remove_tree "$item"
	done
}

cleanup() {
	local name status swept rest
	if [ -n "$T" ] && [ -x "$T/bin/dshd" ] && [ -n "${kit_env+set}" ]; then
		for name in $names; do
			if [ -e "$agents/dshd.$name.plist" ]; then
				call_dshd "$timeout" disable "$name" --json
				check 9 "dshd disable $name" "$(last_error)" "$last_status"
			fi
			[ -d "$state_root/$name" ] || continue
			call_dshd $((timeout + 40)) stop "$name" --json
			status=$?
			case $status in
			0 | 5) record 9 PASS "dshd stop $name" "exit $status" ;;
			*) record 9 FAIL "dshd stop $name" "$(last_error)" ;;
			esac
			call_dshd "$timeout" rm "$name" --json
			check 9 "dshd rm $name" "$(last_error)" "$last_status"
			if [ -e "$state_root/$name" ]; then
				record 9 FAIL "$name's directory is gone"
			fi
		done
		if [ "$release_added" = 1 ]; then
			call_dshd "$timeout" release rm "$prefix" --json
			case $last_status in
			0) record 9 PASS "dshd release rm $prefix" ;;
			2) record 9 SKIP "dshd release rm $prefix" "no such verb: $(last_error)" ;;
			*) record 9 FAIL "dshd release rm $prefix" "$(last_error)" ;;
			esac
			call_dshd "$timeout" release prune --json
			check 9 "dshd release prune" "$(last_error)" "$last_status"
			rest=$(existing "$state_root/.releases/"*"-dshd-release-$prefix")
			check 9 "the release entry is gone" "${rest:+still there: $(basename "$rest") (held by the release name $prefix?)}" "$([ -z "$rest" ] && echo 0)"
			if call_dshd "$timeout" release ls --json; then
				if grep -q "\"$prefix\"" "$kit/out"; then
					record 9 FAIL "release ls no longer names $prefix" "no dshd verb removes a release name"
				else
					record 9 PASS "release ls no longer names $prefix"
				fi
			else
				record 9 SKIP "release ls no longer names $prefix" "$(last_error)"
			fi
		fi
	fi
	swept=$(sweep)
	if [ "$cleaning" = 1 ]; then
		record 9 INFO "removed by the sweep" "${swept:-nothing}"
	elif [ -z "$swept" ]; then
		record 9 PASS "nothing was left for the sweep"
	else
		record 9 FAIL "nothing was left for the sweep" "removed: $(printf '%s' "$swept" | tr '\n' ' ')"
	fi
	if [ "$releases_existed" = 0 ] && [ -d "$state_root/.releases" ]; then
		rmdir "$state_root/.releases" 2>/dev/null || record 9 FAIL "the state root's .releases is removed" "not empty: $(entries_of "$state_root/.releases")"
	fi
	if [ "$state_root_existed" = 0 ] && [ -d "$state_root" ]; then
		rmdir "$state_root" 2>/dev/null || record 9 FAIL "the state root is removed" "not empty: $(entries_of "$state_root")"
		rmdir "$home/.local/state" "$home/.local" 2>/dev/null
	fi
	if [ "$agents_existed" = 0 ] && [ -d "$agents" ]; then
		rmdir "$agents" 2>/dev/null
	fi
	if [ -n "$T" ]; then
		rm -f "$kit/state"
		remove_tree "$T"
	fi
	rest=$(leftovers)
	[ -z "$T" ] || [ ! -e "$T" ] || rest="$rest $T"
	if [ "$state_root_existed" = 0 ] && [ -e "$state_root" ]; then
		rest="$rest $state_root"
	fi
	check 9 "nothing of the kit remains" "$(printf '%s' "$rest" | tr '\n' ' ')" "$([ -z "$(printf '%s' "$rest" | tr -d ' ')" ] && echo 0)"
}

# When the kit stops before its end (an error in the kit, an interrupt),
# the clean-up still runs.
on_exit() {
	local status=$?
	set +u
	if [ "$done_or_held" = 0 ] && [ -n "$T" ] && [ -d "$T" ]; then
		done_or_held=1
		record 9 FAIL "the kit ran to its end" "it stopped early (exit $status): cleaning up"
		cleanup
		print_table
		status=1
	fi
	rm -f "$rows" "$rows.labels"
	exit "$status"
}

finish() {
	done_or_held=1
	print_table
	[ "$failures" = 0 ] || exit 1
	exit 0
}

# --- modes -------------------------------------------------------------------

new_run_steps() {
	preflight
	setup
	step_binaries
	if ! step_release; then
		record 3 SKIP "steps 3 to 8" "need a release"
		return 1
	fi
	if [ "$gui" = 0 ]; then
		step_no_gui
		record 3 SKIP "steps 3 to 7" "need a GUI login session"
		return 1
	fi
	if ! step_web; then
		record 4 SKIP "steps 4 to 7" "need a running web instance"
		return 1
	fi
	step_detached
}

remaining_steps() {
	step_stop_start
	step_crash
	step_killed_runner
	step_ended_capture
	step_domain_capture
}

hold() {
	local status=0
	new_run_steps || status=1
	if [ "$status" = 0 ] && [ "$hold_for" = logout ]; then
		step_enable
	fi
	if [ "$status" = 1 ] || [ "$failures" != 0 ]; then
		record 9 INFO "the hold ends here" "a step failed: cleaning up"
		cleanup
		finish
	fi
	save_state
	cp "$rows" "$kit/rows"
	done_or_held=1
	print_table
	printf '\n'
	case $hold_for in
	disconnect)
		printf 'Now close this Terminal window or SSH session. Open a new one, then run:\n'
		;;
	logout)
		printf 'Now log out at the screen (Apple menu, Log Out) and log in again. Note any\n'
		printf 'Background Items notification. Then, from any Terminal or SSH session, run:\n'
		;;
	esac
	printf '\n  %s resume %s\n\nTo give up instead: %s clean %s\n' "$0" "$T" "$0" "$T"
	exit 0
}

resume() {
	local state now
	load_state "$1"
	cat "$kit/rows" >>"$rows"
	failures=$(grep -c "${tab}FAIL${tab}" "$rows")
	set_environment
	get "$prefix-web"
	state=$(field state)
	now=$(field invocationId)
	case $hold_for in
	disconnect)
		check 4 "after the session closed: still running" "$state" "$([ "$state" = running ] && echo 0)"
		check 4 "after the session closed: the same invocation" "$now" "$([ "$now" = "$invocation" ] && echo 0)"
		;;
	logout)
		check 7 "after logout and login: running" "$state" "$([ "$state" = running ] && echo 0)"
		check 7 "after logout and login: a new invocation" "$now" "$([ -n "$now" ] && [ "$now" != "$invocation" ] && echo 0)"
		if grep -r -l -F "$invocation" "$state_root/$prefix-web" 2>/dev/null | xargs grep -l -F stopped-without-intent >/dev/null 2>&1; then
			record 7 INFO "the earlier run's record says stopped-without-intent"
		else
			record 7 INFO "the earlier run's ending" "not visible: get shows only the latest run"
		fi
		step_disable
		;;
	esac
	invocation=$now
	mainpid=$(field mainPid)
	save_state
	remaining_steps
	cleanup
	finish
}

clean() {
	local dir found=0
	cleaning=1
	if [ -n "${1-}" ]; then
		load_state "$1"
		set_environment
	fi
	for dir in /tmp/dshd-"$prefix".*; do
		[ -f "$dir/kit/state" ] || continue
		[ "$(cd -P "$dir" && pwd)" != "$T" ] || continue
		found=1
		record 9 INFO "another temporary prefix of the kit" "$dir: removed"
		remove_tree "$dir"
	done
	[ "$found" = 1 ] || [ -n "$T" ] || record 9 INFO "no temporary prefix of the kit was found"
	cleanup
	finish
}

# --- arguments ---------------------------------------------------------------

[ "$#" -gt 0 ] || usage
mode=$1
shift
case $mode in
run | hold | resume | clean) ;;
-h | --help | help) usage ;;
*) usage ;;
esac
positional=
while [ "$#" -gt 0 ]; do
	case $1 in
	--dshd) dshd_in=${2-}; shift ;;
	--dshd-run) runner_in=${2-}; shift ;;
	--dsh) dsh_in=${2-}; shift ;;
	--prefix) prefix=${2-}; shift ;;
	--path) extra_path=${2-}; shift ;;
	--own-dsh-home) own_dsh_home=1 ;;
	--captures) captures=${2-}; shift ;;
	--capture-domain) capture_domain=1 ;;
	--timeout) timeout=${2-}; shift ;;
	--for) hold_for=${2-}; shift ;;
	-h | --help) usage ;;
	-*) die "unknown option $1" ;;
	*)
		[ -z "$positional" ] || die "one DIR only"
		positional=$1
		;;
	esac
	shift
done

uid=$(id -u)
user=$(id -un)
host=$(hostname -s 2>/dev/null || hostname)
home=${DSHD_KIT_HOME:-}
if [ -z "$home" ]; then
	home=$(dscl . -read "/Users/$user" NFSHomeDirectory 2>/dev/null | sed -n 's/^NFSHomeDirectory: //p')
	home=${home:-$HOME}
fi
state_root=$home/.local/state/dshd
agents=$home/Library/LaunchAgents

case $prefix in
'' | *[!a-z]*) die "--prefix takes 1 to 8 letters a-z" ;;
esac
[ "${#prefix}" -le 8 ] || die "--prefix takes 1 to 8 letters a-z"
case $timeout in
'' | *[!0-9]*) die "--timeout takes whole seconds" ;;
esac

case $mode in
run | hold)
	[ -z "$positional" ] || die "$mode takes no DIR"
	[ -f "$dshd_in" ] || die "--dshd FILE is required"
	[ -e "$dsh_in" ] || die "--dsh FILE is required"
	if [ -z "$runner_in" ]; then
		for runner_in in "$(dirname "$dshd_in")/../libexec/dshd/dshd-run" "$(dirname "$dshd_in")/dshd-run"; do
			[ ! -f "$runner_in" ] || break
		done
	fi
	[ -f "$runner_in" ] || die "no dshd-run found; give --dshd-run FILE"
	if [ -n "$captures" ]; then
		mkdir -p "$captures" || die "cannot make $captures"
		captures=$(cd -P "$captures" && pwd)
	fi
	if [ "$mode" = hold ]; then
		case $hold_for in
		disconnect | logout) ;;
		*) die "hold needs --for disconnect or --for logout" ;;
		esac
		hold
	fi
	if new_run_steps; then
		record 4 SKIP "a real disconnect" "hold --for disconnect, then resume"
		remaining_steps
		record 7 SKIP "logout and login" "hold --for logout, then resume"
	fi
	[ "$gui" = 0 ] || record 8 SKIP "no GUI login session" "run over SSH while nobody is logged in at the screen"
	cleanup
	finish
	;;
resume)
	[ -n "$positional" ] || die "resume needs the DIR the hold printed"
	resume "$positional"
	;;
clean)
	clean "$positional"
	;;
esac
