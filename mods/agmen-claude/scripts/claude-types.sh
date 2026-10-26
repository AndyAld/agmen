#!/usr/bin/env bash
# Lays the installed Claude Code's plugin API types where tsconfig.json reads
# them: .claude-plugin/types/ in this mod, which git ignores. The types are
# Anthropic's and change with every Claude Code release, so they are produced
# at check time and never committed (ruling R41). npm runs this before
# `npm run typecheck`.
#
# Two sources, both written by the installed Claude Code itself:
#   1. The engine lays .claude-plugin/types/ each time an interactive session
#      loads this mod from a folder it hot-reloads:
#        claude --plugin-dir mods/agmen-claude
#      When that copy names the installed version, it is used as it is.
#   2. Otherwise this copies the declarations (API, testing kit and built-in
#      tool inputs in one file) that the same build writes when a session loads
#      its plugin-authoring skill, under
#        /tmp/claude-<uid>/bundled-skills/<version>/<hash>/plugin-authoring/types/
#      newest first, into .claude-plugin/types/claude-code/index.d.ts.
# With neither, it says how to get one and exits 1, so tsc never runs against
# missing or stale types.
set -euo pipefail

mod=$(cd "$(dirname "$0")/.." && pwd)
types="$mod/.claude-plugin/types"
api="$types/claude-code/index.d.ts"

if ! version_line=$(claude --version 2>/dev/null); then
	echo "claude-types: claude is not on PATH; install Claude Code first." >&2
	exit 1
fi
version=${version_line%% *}
if [[ ! $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
	echo "claude-types: cannot read a version from 'claude --version': $version_line" >&2
	exit 1
fi
# The engine's first line of every types file it writes.
stamp="// Written by Claude Code $version."

if [[ -f $api && "$(head -n 1 "$api")" == "$stamp" ]]; then
	exit 0
fi

newest=
for file in "/tmp/claude-$(id -u)/bundled-skills/$version"/*/plugin-authoring/types/claude-code.d.ts; do
	[[ -f $file && "$(head -n 1 "$file")" == "$stamp" ]] || continue
	if [[ -z $newest || $file -nt $newest ]]; then
		newest=$file
	fi
done

if [[ -z $newest ]]; then
	cat >&2 <<EOF
claude-types: no Claude Code $version types found for $mod.
Load the mod once in an interactive session, which lays them:
  claude --plugin-dir $mod
or load the plugin-authoring skill in any session, then run this again.
EOF
	exit 1
fi

# A folder an older engine laid holds stale siblings (built-in tools, MCP
# tools) that would clash with the new declarations: start it over.
rm -rf "$types"
mkdir -p "$(dirname "$api")"
cp "$newest" "$api"
echo "claude-types: laid Claude Code $version types from $newest"
