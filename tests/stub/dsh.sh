#!/bin/sh
# The stub dsh: the one fake DSH used by cargo tests, the cli check and the
# VM checks. It follows the pinned launcher grammar closely enough for
# dshd's argv scan to be exercised for real, and it is driven by environment
# variables so no check needs a different script:
#
#   DSH_STUB_MODE   web | acp | acp-stalled-input | headless | crash-after-ready | hang | exit-N
#                   (unset: web or acp by the profile's bundles, else
#                   headless)
#   DSH_STUB_PORT   web: the port to announce when argv has no --port N
#                   (unset: one nothing listens on)
#   DSH_STUB_LOG    file each invocation appends its record to
#   DSH_STUB_GATE   acp: a prompt containing "gate" waits until this file
#                   exists, then removes it
#   DSH_STUB_DELAY  headless: seconds to sleep after printing, before the
#                   exit, so a task is still running when a check looks
#   DSH_STUB_BACKGROUND
#                   web and headless, when 1: before anything else, starts
#                   a process that ignores SIGHUP and SIGTERM (nohup, then
#                   sleep 600, stdio on /dev/null so it holds no pipe of
#                   dsh's) and logs its pid, as a tool dsh started would
#   DSH_STUB_RUN_AFTER_READY
#                   web: one shell command run after the announcement (an
#                   agent acting on its own instance); its status is logged
#
# Grammar honoured: a profile word as the bare first token, --profile V,
# --profile=V, --patch V and --patch=V. -V and --version print a version.
# Everything after the launcher flags is the app and its arguments.
# --dump-default-config materialises a shipped profile as DSH does when it
# has no package.json yet: it writes $DSH_HOME/profiles/<profile>/package.json
# with the shipped bundles of web, acp, headless, sdk or sdk-minimal, and
# exits; for any other name it prints DSH's refusal and exits 1. Booting
# writes nothing under $DSH_HOME.
#
# The record of one invocation in DSH_STUB_LOG, in order:
#   invocation PID
#   argc N
#   argv ARG1 NUL ARG2 NUL ... ARGN NUL, then a newline
#   sigblk MASK                    (the blocked-signal mask dsh was given, hex)
#   cwd DIR
#   nofile SOFT HARD               (the open-files limits dsh was given)
#   env NAME=VALUE, one line per variable, sorted
#   mode PID MODE
#   background PID                 (DSH_STUB_BACKGROUND: the process started)
#   web PID PORT                   (web: the announced port)
#   after-ready PID STATUS         (DSH_STUB_RUN_AFTER_READY: its status)
#   acp PID METHOD ID              (acp: each request or notification)
#   acp PID response ID OUTCOME    (acp: each answer to a permission)
#   acp PID flooded                (acp: a flood was written in full)
#
# web: prints "dsh web: http://127.0.0.1:PORT/?token=stubPID" and waits
#   for SIGTERM or SIGINT, then exits 0. Nothing listens on the port; an
#   explicit --port N held by a listener in /proc/net/tcp is "address in
#   use", exit 1 before the announcement, as DSH does.
# crash-after-ready: web, then two seconds after the announcement prints
#   "stub: crashing" on stdout and exits 1.
# hang: no announcement; waits for SIGTERM or SIGINT.
# headless: prints the app arguments once and exits 0.
# exit-N: exits N at once.
# acp: newline-delimited JSON-RPC on stdio, exits 0 at EOF. It answers
#   initialize (counting them in _meta.initializeCount), session/new (ids
#   stub-1, stub-2, ...), session/list (only sessions not in use, as DSH
#   does), session/resume (refused for a session in use, as DSH does),
#   session/close, session/set_config_option (with a config_option_update)
#   and session/prompt: one text chunk "turn N", then end_turn. A prompt
#   containing "permission" sends session/request_permission with id
#   90000+N and ends the turn when it is answered, after a chunk
#   "permission OUTCOME"; session/cancel ends a pending turn "cancelled".
#   A prompt containing "flood" (after the gate when it also contains
#   "gate") writes 4000 chunks "flood K" of about 1 KiB each, K from 1,
#   logs the flood, then ends the turn. Other requests get -32601.

set -u

FLOOD_CHUNKS=4000

log() {
	if [ -n "${DSH_STUB_LOG:-}" ]; then
		printf '%s\n' "$*" >>"$DSH_STUB_LOG"
	fi
}

record_invocation() {
	[ -n "${DSH_STUB_LOG:-}" ] || return 0
	# Read by the shell itself before anything forks, since a shell may block
	# signals around a fork.
	blocked=absent
	while read -r key value; do
		[ "$key" != SigBlk: ] || blocked=$value
	done </proc/self/status
	{
		printf 'invocation %s\nargc %s\nargv ' "$$" "$#"
		if [ "$#" -gt 0 ]; then
			printf '%s\0' "$@"
		fi
		printf '\nsigblk %s\ncwd %s\n' "$blocked" "$(pwd)"
		printf 'nofile %s %s\n' "$(ulimit -Sn)" "$(ulimit -Hn)"
		env | LC_ALL=C sort | sed 's/^/env /'
	} >>"$DSH_STUB_LOG"
}

start_background() {
	[ "${DSH_STUB_BACKGROUND:-}" = 1 ] || return 0
	nohup sh -c 'trap "" HUP TERM; exec sleep 600' </dev/null >/dev/null 2>&1 &
	log "background $!"
}

run_after_ready() {
	[ -n "${DSH_STUB_RUN_AFTER_READY:-}" ] || return 0
	sh -c "$DSH_STUB_RUN_AFTER_READY" </dev/null
	log "after-ready $$ $?"
}

dsh_home() {
	printf '%s' "${DSH_HOME:-${HOME:-}/.dsh}"
}

dump_profile() {
	dir=$(dsh_home)/profiles/$profile
	[ ! -e "$dir/package.json" ] || return 0
	base='"@deepseek-ai/dsh-base"'
	case $profile in
	web) bundles="$base,\"@deepseek-ai/dsh-web-app\"" ;;
	acp) bundles="$base,\"@deepseek-ai/dsh-acp-app\"" ;;
	headless) bundles="$base,\"@deepseek-ai/dsh-headless\"" ;;
	sdk) bundles="$base,\"@deepseek-ai/dsh-sdk-app\"" ;;
	sdk-minimal) bundles='"@deepseek-ai/dsh-sdk-minimal"' ;;
	*)
		printf "dsh: profile \"%s\" does not exist; create it with 'dsh plugin --profile %s add <package>'\n" "$profile" "$profile" >&2
		exit 1
		;;
	esac
	mkdir -p "$dir" || exit 1
	printf '{"name":"%s","dsh":{"profile":{"bundles":[%s]}}}\n' "$profile" "$bundles" >"$dir/package.json"
}

mode_of_profile() {
	manifest=$(dsh_home)/profiles/$profile/package.json
	if [ -f "$manifest" ]; then
		case $(cat "$manifest") in
		*web-app*)
			printf web
			return
			;;
		*acp-app*)
			printf acp
			return
			;;
		esac
	fi
	printf headless
}

listening() {
	hex=$(printf '%04X' "$1")
	cat /proc/net/tcp /proc/net/tcp6 2>/dev/null | grep -q ":$hex [0-9A-F:]* 0A "
}

requested_port() {
	while [ "$#" -gt 0 ]; do
		case $1 in
		--) return ;;
		--port)
			printf '%s' "${2-}"
			return
			;;
		--port=*)
			printf '%s' "${1#--port=}"
			return
			;;
		esac
		shift
	done
}

serve_web() {
	port=$(requested_port "$@")
	if [ -n "$port" ] && [ "$port" != 0 ]; then
		if listening "$port"; then
			printf 'dsh: listen EADDRINUSE: address already in use 127.0.0.1:%s\n' "$port" >&2
			exit 1
		fi
	else
		port=${DSH_STUB_PORT:-$((20000 + $$ % 20000))}
		while listening "$port"; do
			port=$((port + 1))
		done
	fi
	log "web $$ $port"
	printf 'dsh web: http://127.0.0.1:%s/?token=stub%s\n' "$port" "$$"
}

wait_for_stop() {
	trap 'kill "${sleeper-}" 2>/dev/null; exit 0' TERM INT
	sleep 2147483647 </dev/null >/dev/null 2>&1 &
	sleeper=$!
	wait "$sleeper"
	exit 0
}

# The raw value after the first "KEY": in $2, one space allowed after the
# colon: a string's characters between its quotes, else the text up to the
# next comma or closing bracket. Status 1 when the key is absent.
json_value() {
	rest=${2#*\"$1\":}
	[ "$rest" != "$2" ] || return 1
	rest=${rest# }
	case $rest in
	\"*)
		rest=${rest#\"}
		printf '%s' "${rest%%\"*}"
		;;
	*) printf '%s' "${rest%%[],\}]*}" ;;
	esac
}

send() {
	printf '%s\n' "$1"
}

result() {
	send "{\"jsonrpc\":\"2.0\",\"id\":$1,\"result\":$2}"
}

failure() {
	send "{\"jsonrpc\":\"2.0\",\"id\":$1,\"error\":{\"code\":$2,\"message\":\"$3\"}}"
}

chunk() {
	send "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"$1\",\"update\":{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{\"type\":\"text\",\"text\":\"$2\"}}}}"
}

ask_permission() {
	send "{\"jsonrpc\":\"2.0\",\"id\":$1,\"method\":\"session/request_permission\",\"params\":{\"sessionId\":\"$2\",\"toolCall\":{\"toolCallId\":\"call-$1\",\"title\":\"stub tool\"},\"options\":[{\"optionId\":\"allow\",\"name\":\"Allow\",\"kind\":\"allow_once\"},{\"optionId\":\"reject\",\"name\":\"Reject\",\"kind\":\"reject_once\"}]}}"
}

# FLOOD_CHUNKS chunks of about 1 KiB for $session, numbered from 1.
flood() {
	pad=$(printf '%1024s' '' | tr ' ' x)
	sent=0
	while [ "$sent" -lt "$FLOOD_CHUNKS" ]; do
		sent=$((sent + 1))
		chunk "$session" "flood $sent $pad"
	done
	log "acp $$ flooded"
}

wait_gate() {
	[ -n "${DSH_STUB_GATE:-}" ] || return 0
	while [ ! -e "$DSH_STUB_GATE" ]; do
		sleep 0.05
	done
	rm -f "$DSH_STUB_GATE"
}

list_sessions() {
	printf '%s' "$known" | while IFS="$tab" read -r listed cwd; do
		case $active in
		*" $listed "*) ;;
		*) printf '{"sessionId":"%s","cwd":"%s"},' "$listed" "$cwd" ;;
		esac
	done
}

prompt() {
	turn=$((turn + 1))
	chunk "$session" "turn $turn"
	case $line in
	*permission*)
		pending_prompt=$id
		pending_permission=$((90000 + turn))
		pending_session=$session
		ask_permission "$pending_permission" "$session"
		;;
	*flood*)
		case $line in
		*gate*) wait_gate ;;
		esac
		flood
		result "$id" '{"stopReason":"end_turn"}'
		;;
	*gate*)
		wait_gate
		chunk "$session" "after gate $turn"
		result "$id" '{"stopReason":"end_turn"}'
		;;
	*) result "$id" '{"stopReason":"end_turn"}' ;;
	esac
}

answered() {
	case $line in
	*'"cancelled"'*) outcome=cancelled ;;
	*) outcome=$(json_value optionId "$line") || outcome=none ;;
	esac
	log "acp $$ response $id $outcome"
	[ -n "$pending_permission" ] && [ "$id" = "$pending_permission" ] || return 0
	chunk "$pending_session" "permission $outcome"
	if [ "$outcome" = cancelled ]; then
		result "$pending_prompt" '{"stopReason":"cancelled"}'
	else
		result "$pending_prompt" '{"stopReason":"end_turn"}'
	fi
	pending_prompt=
	pending_permission=
	pending_session=
}

acp_request() {
	case $method in
	initialize)
		initialized=$((initialized + 1))
		result "$id" "{\"protocolVersion\":1,\"agentCapabilities\":{\"loadSession\":false,\"sessionCapabilities\":{\"list\":{},\"resume\":{},\"close\":{}}},\"_meta\":{\"initializeCount\":$initialized}}"
		;;
	session/new)
		sessions=$((sessions + 1))
		cwd=$(json_value cwd "$line") || cwd=
		active="${active}stub-$sessions "
		known="${known}stub-$sessions$tab$cwd$newline"
		result "$id" "{\"sessionId\":\"stub-$sessions\",\"configOptions\":$config}"
		;;
	session/resume)
		case $active in
		*" $session "*) failure "$id" -32602 "session $session is already active" ;;
		*)
			case $known in
			*"$session$tab"*)
				active="$active$session "
				result "$id" "{\"configOptions\":$config}"
				;;
			*) failure "$id" -32602 "unknown session $session" ;;
			esac
			;;
		esac
		;;
	session/list)
		entries=$(list_sessions)
		result "$id" "{\"sessions\":[${entries%,}]}"
		;;
	session/close)
		case $active in
		*" $session "*)
			before=${active%%" $session "*}
			after=${active#*" $session "}
			active="$before $after"
			;;
		esac
		result "$id" '{}'
		;;
	session/set_config_option)
		value=$(json_value value "$line") || value=default
		config="[{\"id\":\"mode\",\"currentValue\":\"$value\"}]"
		send "{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{\"sessionId\":\"$session\",\"update\":{\"sessionUpdate\":\"config_option_update\",\"configOptions\":$config}}}"
		result "$id" "{\"configOptions\":$config}"
		;;
	session/prompt) prompt ;;
	*) failure "$id" -32601 "method not found: $method" ;;
	esac
}

acp_loop() {
	tab=$(printf '\t')
	newline='
'
	config='[{"id":"mode","currentValue":"default"}]'
	initialized=0
	sessions=0
	turn=0
	active=' '
	known=
	pending_prompt=
	pending_permission=
	pending_session=
	while IFS= read -r line || [ -n "$line" ]; do
		method=$(json_value method "$line") || method=
		id=$(json_value id "$line") || id=
		session=$(json_value sessionId "$line") || session=
		if [ -z "$method" ]; then
			[ -z "$id" ] || answered
			continue
		fi
		log "acp $$ $method ${id:--}"
		if [ -n "$id" ]; then
			acp_request
		elif [ "$method" = session/cancel ] && [ -n "$pending_prompt" ] && [ "$session" = "$pending_session" ]; then
			result "$pending_prompt" '{"stopReason":"cancelled"}'
			pending_prompt=
			pending_permission=
			pending_session=
		fi
	done
	exit 0
}

record_invocation "$@"

profile=default
dump=
if [ "$#" -gt 0 ]; then
	case $1 in
	-*) ;;
	*)
		profile=$1
		shift
		;;
	esac
fi
while [ "$#" -gt 0 ]; do
	case $1 in
	--profile | --patch)
		if [ "$#" -lt 2 ]; then
			printf 'dsh: %s needs a value\n' "$1" >&2
			exit 2
		fi
		[ "$1" = --patch ] || profile=$2
		shift 2
		;;
	--profile=*)
		profile=${1#--profile=}
		shift
		;;
	--patch=*) shift ;;
	--dump-default-config)
		dump=1
		shift
		;;
	-V | --version)
		printf 'dsh stub\n'
		exit 0
		;;
	*) break ;;
	esac
done

if [ -n "$dump" ]; then
	dump_profile
	exit 0
fi

mode=${DSH_STUB_MODE:-$(mode_of_profile)}
log "mode $$ $mode"
case $mode in
web)
	start_background
	serve_web "$@"
	run_after_ready
	wait_for_stop
	;;
crash-after-ready)
	serve_web "$@"
	sleep 2
	printf 'stub: crashing\n'
	exit 1
	;;
hang) wait_for_stop ;;
headless)
	start_background
	printf '%s\n' "$*"
	if [ -n "${DSH_STUB_DELAY:-}" ]; then
		sleep "$DSH_STUB_DELAY"
	fi
	exit 0
	;;
acp-stalled-input)
	printf '{"jsonrpc":"2.0","method":"dshd/stalled-input"}\n'
	wait_for_stop
	;;
acp) acp_loop ;;
exit-*) exit "${mode#exit-}" ;;
*)
	printf 'dsh stub: unknown DSH_STUB_MODE %s\n' "$mode" >&2
	exit 64
	;;
esac
