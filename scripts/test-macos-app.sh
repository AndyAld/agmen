#!/bin/sh
# Build and inspect an isolated app without opening a window or starting agmend.
set -eu
cd "$(dirname "$0")/.."
repo=$PWD
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT HUP INT TERM
version=$(cargo metadata --no-deps --locked --format-version 1 | jq -r '.packages[] | select(.name == "agmen-desktop") | .version')
sh scripts/macos-app.sh build --version "$version" --build 1 --out "$scratch/out"
app=$scratch/out/agmen.app
plutil -lint "$app/Contents/Info.plist"
[ "$(plutil -extract CFBundleShortVersionString raw "$app/Contents/Info.plist")" = "$version" ]
[ "$(plutil -extract CFBundleIdentifier raw "$app/Contents/Info.plist")" = ai.getagmen.desktop ]
[ "$(plutil -extract LSMinimumSystemVersion raw "$app/Contents/Info.plist")" = 13.0 ]
[ "$(plutil -extract CFBundleVersion raw "$app/Contents/Info.plist")" = 1 ]
[ "$(plutil -extract CFBundleIconFile raw "$app/Contents/Info.plist")" = AppIcon ]
installed=$(rustup target list --installed)
if printf '%s\n' "$installed" | rg -qx x86_64-apple-darwin && printf '%s\n' "$installed" | rg -qx aarch64-apple-darwin; then
	expected_archs='arm64 x86_64'
else
	case $(rustc -vV | sed -n 's/^host: //p') in
	aarch64-apple-darwin) expected_archs=arm64 ;;
	x86_64-apple-darwin) expected_archs=x86_64 ;;
	*)
		echo 'Unsupported host' >&2
		exit 1
		;;
	esac
fi
for binary in agmen-desktop agmend agmen; do
	archs=$(lipo -archs "$app/Contents/MacOS/$binary")
	if [ "$archs" = 'x86_64 arm64' ]; then archs='arm64 x86_64'; fi
	[ "$archs" = "$expected_archs" ] || {
		echo "Expected $expected_archs, got $archs" >&2
		exit 1
	}
	echo "$binary architectures: $archs"
done
[ "$("$app/Contents/MacOS/agmen-desktop" --version)" = "agmen-desktop $version (1)" ]
"$app/Contents/MacOS/agmend" --help
[ "$("$app/Contents/MacOS/agmen" --version)" = "agmen $version" ]
for resource in LICENSE THIRD_PARTY_NOTICES.md THIRD_PARTY_NOTICES-desktop.md AppIcon.icns; do
	test -s "$app/Contents/Resources/$resource"
done
cmp "$repo/crates/agmen-desktop/assets/icon/agmen.icns" "$app/Contents/Resources/AppIcon.icns"
echo 'PASS: bundle layout, versions, architectures, icon and notices'
sh scripts/macos-app.sh sign --app "$app"
for binary in agmend agmen; do
	codesign --verify --strict --verbose=2 "$app/Contents/MacOS/$binary"
done
codesign --verify --strict --verbose=2 "$app"
sh scripts/macos-app.sh package --app "$app" --out "$scratch/out"
(cd "$scratch/out" && shasum -a 256 -c SHA256SUMS)
test -s "$scratch/out/agmen-$version.dmg"
test -s "$scratch/out/agmen-$version-update.zip"
identity=${AGMEN_SIGN_IDENTITY:--}
if [ "$identity" != - ]; then
	codesign -dv --verbose=4 "$app" 2>"$scratch/signature.txt"
	cat "$scratch/signature.txt"
	rg 'flags=.*runtime' "$scratch/signature.txt"
	rg '^TeamIdentifier=.' "$scratch/signature.txt"
fi
if security find-identity -v -p codesigning | rg -q 'Developer ID Application:' && [ -n "${AGMEN_NOTARY_PROFILE:-}" ] && printf '%s\n' "$identity" | rg -q '^Developer ID Application:'; then
	spctl --assess --type execute -vv "$app"
	xcrun stapler validate "$app"
	xcrun stapler validate "$scratch/out/agmen-$version.dmg"
	spctl --assess --type open --context context:primary-signature -vv "$scratch/out/agmen-$version.dmg"
else
	echo 'SKIP: notarized Developer ID acceptance requires that identity and AGMEN_NOTARY_PROFILE'
fi
echo 'PASS: signing, DMG, update archive and checksums'
