#!/bin/sh
# The fake nix-store of the golden CLI cases: appends its arguments,
# space-joined, as one line to $FAKE_DIR/log/nix-store, then
#
#   --realise PATH [--add-root LINK]
#       fails as nix-store does when PATH does not exist; else points LINK
#       at PATH and prints LINK (PATH without --add-root)
#   --option NAME VALUE
#       accepted and ignored

set -u

printf '%s\n' "$*" >>"$FAKE_DIR/log/nix-store" || exit 1

action=
path=
root=
while [ "$#" -gt 0 ]; do
	case $1 in
	--option)
		shift
		[ "$#" -gt 0 ] && shift
		;;
	--realise | -r) action=realise ;;
	--add-root)
		shift
		root=${1-}
		;;
	-*) ;;
	*) path=$1 ;;
	esac
	[ "$#" -gt 0 ] && shift
done

case $action in
realise)
	if [ ! -e "$path" ]; then
		printf "error: path '%s' is not valid\n" "$path" >&2
		exit 1
	fi
	if [ -n "$root" ]; then
		ln -sfn "$path" "$root" || exit 1
		printf '%s\n' "$root"
	else
		printf '%s\n' "$path"
	fi
	;;
esac
exit 0
