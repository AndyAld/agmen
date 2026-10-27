#!/bin/sh
set -eu
model=offline
stream=false
while [ "$#" -gt 0 ]; do
	case "$1" in
	--model)
		model=$2
		shift
		;;
	--input-format)
		stream=true
		shift
		;;
	esac
	shift
done
for _turn in 1 2 3; do
	if ! IFS= read -r batch; then exit 0; fi
	if [ "$stream" = true ]; then
		case "$batch" in
		*'"role":"user"'*) ;;
		*) exit 9 ;;
		esac
	fi
	case "$model" in
	malformed)
		printf 'not json\n'
		exit 0
		;;
	eof) exit 0 ;;
	failure) exit 7 ;;
	stall)
		IFS= read -r _extra || exit 0
		exit 0
		;;
	oversize)
		printf '%1048577s\n' x
		exit 0
		;;
	esac
	printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","name":"mcp__agmen__answer","input":{"body":"offline"}}]}}'
	printf '%s\n' '{"type":"result","result":"offline batch","total_cost_usd":0.001,"is_error":false}'
	if [ "$stream" = false ]; then
		if IFS= read -r _extra; then exit 8; fi
		exit 0
	fi
done
