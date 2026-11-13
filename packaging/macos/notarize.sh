#!/bin/sh
# Has Apple notarize Eddie.app and writes the zip to give to other people.
#
#   packaging/macos/notarize.sh [Eddie.app]
#
# The app is target/release/Eddie.app unless one is given, and must carry a
# Developer ID signature (EDDIE_SIGN_IDENTITY, see bundle.sh). Apple is
# asked with an App Store Connect API key when EDDIE_NOTARY_KEY (the .p8
# file), EDDIE_NOTARY_KEY_ID and EDDIE_NOTARY_ISSUER are set, as the
# release workflow sets them. Otherwise it is asked with the keychain
# profile EDDIE_NOTARY_PROFILE ("eddie-notary" unless set). Store that once,
# with an app-specific password for the Apple ID:
#
#   xcrun notarytool store-credentials eddie-notary --apple-id <Apple ID> --team-id <team>
#
# Apple's ticket is stapled to the app, so a Mac opens it without asking
# Apple first. The zip lands beside the app as
# Eddie-<version>-macos-<architecture>.zip, and its path is printed.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
app=${1:-${CARGO_TARGET_DIR:-$root/target}/release/Eddie.app}
if [ -n "${EDDIE_NOTARY_KEY:-}" ]; then
    set -- --key "$EDDIE_NOTARY_KEY" --key-id "${EDDIE_NOTARY_KEY_ID:?}" --issuer "${EDDIE_NOTARY_ISSUER:?}"
else
    set -- --keychain-profile "${EDDIE_NOTARY_PROFILE:-eddie-notary}"
fi

die() {
    echo "$1" >&2
    exit 1
}

[ -d "$app" ] || die "No app at $app. Build it with bundle.sh first."

# Apple refuses any other signature, minutes after the upload.
codesign --display --verbose=2 "$app" 2>&1 | grep -q '^Authority=Developer ID Application:' ||
    die "$app has no Developer ID signature. Build it with EDDIE_SIGN_IDENTITY set (see bundle.sh)."

version=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' "$app/Contents/Info.plist")
architecture=$(lipo -archs "$app/Contents/MacOS/eddie" | tr ' ' '-')
zip=$(dirname "$app")/Eddie-$version-macos-$architecture.zip

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# The notary service takes a zip. ditto makes one that keeps the signature.
ditto -c -k --keepParent "$app" "$work/Eddie.zip"
echo "Sending $app to Apple. The answer takes a few minutes." >&2
xcrun notarytool submit "$work/Eddie.zip" "$@" --wait \
    --output-format json >"$work/answer" || die "Apple's notary service was not reached: $(cat "$work/answer")"

# Any answer but Accepted comes with a log that names each file and fault.
if ! grep -q '"status" *: *"Accepted"' "$work/answer"; then
    cat "$work/answer" >&2
    id=$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$work/answer" | head -n 1)
    [ -z "$id" ] || xcrun notarytool log "$id" "$@" >&2 || true
    die "Apple did not notarize $app"
fi

xcrun stapler staple "$app" >&2
xcrun stapler validate "$app" >&2
spctl --assess --type execute --verbose=2 "$app" >&2 ||
    die "Gatekeeper does not accept $app, though Apple notarized it"

# The stapled app is zipped again: a zip cannot carry the ticket itself.
rm -f "$zip"
ditto -c -k --keepParent "$app" "$zip"
echo "$zip"
