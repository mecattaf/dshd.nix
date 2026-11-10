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
#
# Every other argument (--user, -u UNIT, -q, -o cat, --no-pager) is
# accepted and does not change the output.

set -u

printf '%s\n' "$*" >>"$FAKE_DIR/log/journalctl" || exit 1

journal=$FAKE_DIR/journal
lines=
follow=0
while [ "$#" -gt 0 ]; do
	case $1 in
	_SYSTEMD_INVOCATION_ID=*) journal=$FAKE_DIR/invocations/${1#_SYSTEMD_INVOCATION_ID=} ;;
	-n | --lines)
		lines=${2-}
		[ "$#" -gt 1 ] && shift
		;;
	-f | --follow) follow=1 ;;
	--no-tail) lines=all ;;
	esac
	shift
done

if [ "$follow" = 1 ]; then
	if [ "$lines" != 0 ] && [ -f "$journal" ]; then
		cat "$journal"
	fi
	if [ "${FAKE_FOLLOW_END:-}" = 1 ]; then
		printf 'dshd-run: run ended: success\n'
	fi
	exec sleep 600
fi

[ -f "$journal" ] || exit 0

case $lines in
'' | all) cat "$journal" ;;
+*) head -n "${lines#+}" "$journal" ;;
*) tail -n "$lines" "$journal" ;;
esac
exit 0
