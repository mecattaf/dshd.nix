#!/bin/sh
# The fake journalctl of the golden CLI cases: appends its arguments,
# space-joined, as one line to $FAKE_DIR/log/journalctl, then prints a
# journal. With _SYSTEMD_INVOCATION_ID=ID it is $FAKE_DIR/invocations/ID,
# else $FAKE_DIR/journal; a missing file is an empty journal.
#
#   -n +K      the oldest K lines
#   -n K       the newest K lines
#   -f         --no-tail prints the whole journal, -n 0 none of it; the
#              journal is followed until dshd kills this process; at the
#              current end the fake waits, exactly as a live follower does
#   -o json    one entry per line, as journalctl prints them: the journal's
#              .json sibling when it exists, else {"MESSAGE":"LINE"} for
#              each line; -n counts entries
#
# Every other argument (--user, -u UNIT, -q, -o cat, --all,
# --output-fields=..., --no-pager) is accepted and does not change the
# output.

set -u

printf '%s\n' "$*" >>"$FAKE_DIR/log/journalctl" || exit 1

journal=$FAKE_DIR/journal
lines=
follow=0
format=cat
while [ "$#" -gt 0 ]; do
	case $1 in
	_SYSTEMD_INVOCATION_ID=*) journal=$FAKE_DIR/invocations/${1#_SYSTEMD_INVOCATION_ID=} ;;
	-n | --lines)
		lines=${2-}
		[ "$#" -gt 1 ] && shift
		;;
	-o | --output)
		format=${2-}
		[ "$#" -gt 1 ] && shift
		;;
	-f | --follow) follow=1 ;;
	--no-tail) lines=all ;;
	esac
	shift
done

# The journal in the asked form, one entry per line.
entries() {
	if [ "$format" != json ]; then
		cat "$journal"
	elif [ -f "$journal.json" ]; then
		cat "$journal.json"
	else
		sed 's/["\\]/\\&/g; s/.*/{"MESSAGE":"&"}/' "$journal"
	fi
}

present=0
if [ -f "$journal" ] || [ -f "$journal.json" ]; then
	present=1
fi

if [ "$follow" = 1 ]; then
	if [ "$lines" != 0 ] && [ "$present" = 1 ]; then
		entries
	fi
	if [ "${FAKE_FOLLOW_END:-}" = 1 ] && [ "$format" = json ]; then
		printf '{"MESSAGE":"dshd-run: run ended: success"}\n'
	elif [ "${FAKE_FOLLOW_END:-}" = 1 ]; then
		printf 'dshd-run: run ended: success\n'
	fi
	exec sleep 600
fi

[ "$present" = 1 ] || exit 0

case $lines in
'' | all) entries ;;
+*) entries | head -n "${lines#+}" ;;
*) entries | tail -n "$lines" ;;
esac
exit 0
