#!/bin/sh
# Build, sign and package the four executables from this workspace.
set -eu
cd "$(dirname "$0")/.."
repo=$PWD
fail() {
	echo "macos-app: $*" >&2
	exit 1
}
usage() {
	echo 'Usage: macos-app.sh [build|sign|package] [--version V] [--build N] [--out DIR] [--arch universal|arm64] [--app PATH] [--identity NAME] [--notary-profile NAME]'
	echo 'With no subcommand, build, sign and package into target/macos.'
	echo 'AGMEN_SIGN_IDENTITY defaults to ad hoc; AGMEN_NOTARY_PROFILE defaults to skipping notarization.'
}
command=all
case "${1:-}" in
build | sign | package)
	command=$1
	shift
	;;
-h | --help)
	usage
	exit 0
	;;
'') ;;
*) fail 'expected build, sign or package (see --help)' ;;
esac
version=
build=$(date +%s)
out=$repo/target/macos
arch=universal
app=
identity=${AGMEN_SIGN_IDENTITY:--}
# shortcut: keychain-profile auth only; add CI key auth when it can avoid secret argv.
profile=${AGMEN_NOTARY_PROFILE:-}
while [ "$#" -gt 0 ]; do
	[ "$#" -ge 2 ] || fail "missing value for $1"
	case "$1" in
	--version) version=$2 ;;
	--build) build=$2 ;;
	--out) out=$2 ;;
	--arch) arch=$2 ;;
	--app) app=$2 ;;
	--identity) identity=$2 ;;
	--notary-profile) profile=$2 ;;
	*) fail "unknown option: $1" ;;
	esac
	shift 2
done
[ "$(uname -s)" = Darwin ] || fail 'macOS is required'
case "$out" in /*) ;; *) out=$repo/$out ;; esac
if [ -z "$app" ]; then app=$out/agmen.app; fi
case "$app" in /*) ;; *) app=$repo/$app ;; esac
[ -n "$identity" ] || identity=-
case "$arch" in universal | arm64) ;; *) fail 'arch must be universal or arm64' ;; esac
case "$build" in '' | *[!0-9]*) fail 'build must be a positive integer' ;; esac
[ "$build" -gt 0 ] || fail 'build must be a positive integer'
scratch=
trap 'if [ -n "$scratch" ]; then rm -rf "$scratch"; fi' EXIT HUP INT TERM

build_app() {
	metadata=$(cargo metadata --no-deps --locked --format-version 1)
	workspace_version=$(printf '%s' "$metadata" | jq -er '.packages[] | select(.name == "agmen-desktop") | .version')
	if [ -z "$version" ]; then version=$workspace_version; fi
	[ "$version" = "$workspace_version" ] || fail 'version must match the workspace version'
	for package in agmend agmen-cli agmen-hub; do
		helper_version=$(printf '%s' "$metadata" | jq -er --arg name "$package" '.packages[] | select(.name == $name) | .version')
		[ "$helper_version" = "$version" ] || fail "$package version differs from desktop"
	done
	# Values enter sed replacement text and the plist; reject metacharacters.
	printf '%s\n' "$version" | rg -q '^[0-9]+\.[0-9]+\.[0-9]+([+-][A-Za-z0-9.-]+)?$' || fail 'invalid version'
	for resource in LICENSE THIRD_PARTY_NOTICES.md; do
		[ -s "$repo/$resource" ] || fail "missing release resource: $repo/$resource"
	done
	installed=$(rustup target list --installed)
	host=$(rustc -vV | sed -n 's/^host: //p')
	targets=aarch64-apple-darwin
	if [ "$arch" = universal ]; then
		if printf '%s\n' "$installed" | rg -qx x86_64-apple-darwin && printf '%s\n' "$installed" | rg -qx aarch64-apple-darwin; then
			targets='aarch64-apple-darwin x86_64-apple-darwin'
		else
			targets=$host
			echo "NOTE: universal targets are not both installed; building host only ($host). Nothing will be installed."
		fi
	fi
	for target in $targets; do
		MACOSX_DEPLOYMENT_TARGET=13.0 AGMEN_BUILD=$build CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-3} \
			cargo build --release --locked --target "$target" -p agmen-desktop -p agmend -p agmen-cli -p agmen-hub
	done
	app=$out/agmen.app
	[ ! -L "$app" ] || fail 'refusing a symlink at the output app path'
	mkdir -p "$out"
	rm -rf "$app"
	mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
	for binary in agmen-desktop agmend agmen agmen-hub; do
		if [ "$targets" = 'aarch64-apple-darwin x86_64-apple-darwin' ]; then
			lipo -create "$repo/target/aarch64-apple-darwin/release/$binary" "$repo/target/x86_64-apple-darwin/release/$binary" -output "$app/Contents/MacOS/$binary"
		else
			cp "$repo/target/$targets/release/$binary" "$app/Contents/MacOS/$binary"
		fi
	done
	sed -e "s/@VERSION@/$version/g" -e "s/@BUILD@/$build/g" -e 's/@MIN_MACOS@/13.0/g' \
		crates/agmen-desktop/macos/Info.plist.in >"$app/Contents/Info.plist"
	plutil -lint "$app/Contents/Info.plist"
	cp crates/agmen-desktop/assets/icon/agmen.icns "$app/Contents/Resources/AppIcon.icns"
	cp LICENSE THIRD_PARTY_NOTICES.md "$app/Contents/Resources/"
	cp crates/agmen-desktop/THIRD_PARTY_NOTICES.md "$app/Contents/Resources/THIRD_PARTY_NOTICES-desktop.md"
	echo "Built $app"
}

sign_app() {
	[ -f "$app/Contents/Info.plist" ] || fail "missing app: $app"
	if [ "$identity" = - ]; then
		set -- --timestamp=none
		echo 'NOTE: signing ad hoc'
	else
		set -- --options runtime --timestamp
	fi
	for binary in agmend agmen agmen-hub; do
		codesign --force --sign "$identity" "$@" "$app/Contents/MacOS/$binary"
	done
	codesign --force --sign "$identity" "$@" --entitlements "$repo/crates/agmen-desktop/macos/agmen.entitlements" "$app"
}

notarize() {
	archive=$1
	result=$archive.notary.json
	accepted=false
	if xcrun notarytool submit "$archive" --keychain-profile "$profile" --wait --timeout 600s --output-format json >"$result"; then
		status=$(plutil -extract status raw "$result")
		if [ "$status" = Accepted ]; then accepted=true; fi
	fi
	if [ "$accepted" != true ]; then
		id=$(plutil -extract id raw "$result" 2>/dev/null || true)
		if [ -n "$id" ]; then
			xcrun notarytool log "$id" --keychain-profile "$profile" "$archive.notary.log" || true
		fi
		fail "notarization failed; see $result and $archive.notary.log (if available)"
	fi
	xcrun stapler staple "$2"
}

package_app() {
	[ -f "$app/Contents/Info.plist" ] || fail "missing app: $app"
	version=$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")
	printf '%s\n' "$version" | rg -q '^[0-9]+\.[0-9]+\.[0-9]+([+-][A-Za-z0-9.-]+)?$' || fail 'invalid app version'
	mkdir -p "$out"
	if [ -n "$profile" ]; then
		[ "$identity" != - ] || fail 'notarization requires a signing identity'
		rm -f "$out/agmen-$version.zip"
		ditto -c -k --keepParent "$app" "$out/agmen-$version.zip"
		notarize "$out/agmen-$version.zip" "$app"
	else
		echo 'NOTE: AGMEN_NOTARY_PROFILE is unset; skipping notarization and stapling. The DMG is not notarized.'
	fi
	scratch=$(mktemp -d)
	mkdir "$scratch/dmg"
	ditto "$app" "$scratch/dmg/agmen.app"
	ln -s /Applications "$scratch/dmg/Applications"
	dmg=$out/agmen-$version.dmg
	hdiutil create -ov -volname agmen -srcfolder "$scratch/dmg" -format UDZO "$dmg"
	if [ "$identity" = - ]; then
		codesign --force --sign - --timestamp=none "$dmg"
	else
		codesign --force --sign "$identity" --timestamp "$dmg"
	fi
	if [ -n "$profile" ]; then notarize "$dmg" "$dmg"; fi
	rm -f "$out/agmen-$version-update.zip"
	ditto -c -k --keepParent "$app" "$out/agmen-$version-update.zip"
	(cd "$out" && shasum -a 256 "agmen-$version.dmg" "agmen-$version-update.zip" >SHA256SUMS)
	echo "Packaged $dmg"
}
case "$command" in
build) build_app ;;
sign) sign_app ;;
package) package_app ;;
all)
	build_app
	sign_app
	package_app
	;;
esac
