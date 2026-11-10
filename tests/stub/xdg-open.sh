#!/bin/sh
# The fake xdg-open of the golden CLI cases: appends its arguments,
# space-joined, as one line to $FAKE_DIR/log/xdg-open and opens nothing.

printf '%s\n' "$*" >>"$FAKE_DIR/log/xdg-open" || exit 1
exit 0
