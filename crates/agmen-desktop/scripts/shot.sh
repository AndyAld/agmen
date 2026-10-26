#!/usr/bin/env bash
# Captures the agmen-desktop window showing a mockup screen's fixture state.
#
#   scripts/shot.sh <screen>...   one or more names, such as chat needs-you
#   scripts/shot.sh all           every screen
#   scripts/shot.sh view:code     one view alone (agmen-desktop --view code)
#
# Shots land in target/shots/<screen>.png at the workspace root. Only the app's
# own window is captured (by window id), never the rest of the screen. Needs
# Screen Recording permission for the terminal that runs it. With AeroSpace
# running, the window is floated first so the tiling layout does not resize it.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
out="$root/target/shots"
app="$root/target/debug/agmen-desktop"
tries=40 # about 10 s for the window to appear

if [[ $# -eq 0 ]]; then
	echo "usage: $0 <screen>... | all" >&2
	exit 2
fi
# A debug build: shots do not depend on speed, and it builds much faster.
(cd "$root" && cargo build -p agmen-desktop --locked --quiet)
screens=("$@")
if [[ $1 == all ]]; then
	# The app's own list, so this script never drifts from Screen::ALL.
	# Read the list first, so a failing binary stops the script (set -e)
	# instead of yielding no screens; bash 3.2 (macOS) has no mapfile.
	list="$("$app" --list-screens)"
	screens=()
	while IFS= read -r name; do
		screens+=("$name")
	done <<<"$list"
fi
mkdir -p "$out"
work="$(mktemp -d)"
pid=""
cleanup() {
	if [[ -n $pid ]]; then
		kill "$pid" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT
# Compile the window finder once instead of interpreting it on every poll.
swiftc -O -o "$work/window-id" "$here/window-id.swift"

for screen in "${screens[@]}"; do
	# "view:<name>" shows one view alone (agmen-desktop --view <name>).
	if [[ $screen == view:* ]]; then
		"$app" --view "${screen#view:}" >"$out/$screen.log" 2>&1 &
	else
		"$app" --screen "$screen" >"$out/$screen.log" 2>&1 &
	fi
	pid=$!
	id=""
	for ((i = 0; i < tries; i++)); do
		if ! kill -0 "$pid" 2>/dev/null; then
			echo "$screen: the app exited; see $out/$screen.log" >&2
			pid=""
			exit 1
		fi
		if id="$("$work/window-id" "$pid")"; then
			break
		fi
		sleep 0.25
	done
	if [[ -z $id ]]; then
		echo "$screen: no window appeared; see $out/$screen.log" >&2
		exit 1
	fi
	if command -v aerospace >/dev/null; then
		aerospace layout floating --window-id "$id" >/dev/null 2>&1 || true
	fi
	sleep 2
	screencapture -x -o -l"$id" "$out/$screen.png"
	kill "$pid" 2>/dev/null || true
	wait "$pid" 2>/dev/null || true
	pid=""
	echo "$out/$screen.png"
done
