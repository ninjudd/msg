#!/bin/sh
#
# Build `msg` and `msgd`, and wrap the daemon in a signed app bundle.
#
# The daemon is its own executable so that TCC's client is `msgd` rather than a
# shared runtime, and so that the code holding Full Disk Access cannot be swapped
# out from under the grant (docs/projects/all/daemon-and-permissions.md §4).
#
# It lives in a bundle so that TCC keys its grants by bundle identifier rather
# than by executable path. Path-keyed grants cannot be switched off — System
# Settings only ever creates and deletes them — so a bare executable could be
# granted Automation and then never revoked from the pane (§13).
#
# This replaced a Node build of esbuild, `--experimental-sea-config`, postject,
# a sentinel fuse, and 200 lines of Mach-O load-command arithmetic. What is left
# is a compile, a directory, and a signature, which is what rust-rewrite.md §1
# predicted would be left.

set -eu

# One constant behind both CFBundleIdentifier and `codesign --identifier`, so the
# two can never disagree. Every TCC grant the daemon holds names it.
IDENTIFIER="com.ninjudd.msgd"

# The certificate the grant is anchored to. An ad-hoc signature is matched by
# cdhash, so every rebuild would invalidate Full Disk Access and it would have to
# be granted again by hand. A stable identity anchors the requirement to a
# certificate instead, and rebuilds keep the grant (§4). Nothing here talks to
# Apple: codesign is offline and a self-signed certificate needs no account.
IDENTITY_NAME="msg dev"

CERTIFICATE_DAYS=3650

# LibreSSL, not whatever is on PATH. OpenSSL 3 writes PKCS#12 with defaults the
# macOS Security framework rejects with "MAC verification failed", which reads as
# a wrong password and is not one.
OPENSSL=/usr/bin/openssl

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
out="$root/build"
app="$out/msgd.app"

die() {
	echo "$*" >&2
	exit 1
}

# Identity names codesign can see, valid or not. Not `-v`: a self-signed
# certificate is not "valid", and signs anyway.
has_identity() {
	security find-identity -p codesigning 2>/dev/null |
		grep -q "\"$1\""
}

# Two things this deliberately does not do:
#
#   - No `add-trusted-cert`. Signing works with an untrusted self-signed
#     certificate — measured, codesign exits 0 while `security find-identity`
#     still reports CSSMERR_TP_NOT_TRUSTED. Trust settings would need
#     authorisation and buy nothing here.
#   - No `set-key-partition-list`. Leaving it unset means macOS asks before
#     codesign uses the key. That prompt is the only thing stopping local code
#     from silently signing a replacement daemon and inheriting its grant, which
#     would walk back the scope reduction the daemon exists for (§6). Answering
#     it with "Always Allow" trades that away.
create_identity() {
	name=$1
	work=$(mktemp -d "${TMPDIR:-/tmp}/msg-identity-XXXXXX")
	# The passphrase only protects the file between these two commands.
	passphrase=$("$OPENSSL" rand -hex 16)
	trap 'rm -rf "$work"' EXIT

	cat >"$work/openssl.cnf" <<-EOF
		[req]
		distinguished_name = dn
		x509_extensions = v3
		prompt = no
		[dn]
		CN = $name
		[v3]
		basicConstraints = critical,CA:false
		keyUsage = critical,digitalSignature
		extendedKeyUsage = critical,codeSigning
	EOF

	"$OPENSSL" req -x509 -newkey rsa:2048 -sha256 -days "$CERTIFICATE_DAYS" \
		-nodes -keyout "$work/key.pem" -out "$work/cert.pem" \
		-config "$work/openssl.cnf" 2>/dev/null
	"$OPENSSL" pkcs12 -export -inkey "$work/key.pem" -in "$work/cert.pem" \
		-name "$name" -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 \
		-out "$work/identity.p12" -passout "pass:$passphrase"

	# `security` prints the default keychain indented and quoted. Take what is
	# between the quotes and change nothing inside it: a keychain named
	# "My Login.keychain-db" is perfectly valid, and deleting every space would
	# turn its path into one that does not exist.
	keychain=$(security default-keychain -d user 2>/dev/null |
		sed -n 's/^[[:space:]]*"\(.*\)"[[:space:]]*$/\1/p')
	[ -n "$keychain" ] || keychain="$HOME/Library/Keychains/login.keychain-db"

	# -T lets codesign reach the key without naming every other tool.
	security import "$work/identity.p12" -k "$keychain" -P "$passphrase" \
		-T /usr/bin/codesign >/dev/null

	rm -rf "$work"
	trap - EXIT
	echo "created a \"$name\" code signing certificate in your login keychain"
}

identity=${MSG_SIGN_IDENTITY-}
if [ -z "$identity" ]; then
	identity=$IDENTITY_NAME
	if ! has_identity "$identity"; then
		create_identity "$identity"
		has_identity "$identity" ||
			die "created a certificate named $identity but codesign cannot see it"
	fi
fi

version=$(awk -F'"' '/^version = /{print $2; exit}' "$root/Cargo.toml")

# NSAppleEventsUsageDescription is not decoration. macOS denies an Apple Event
# from a client that has none with -1743, without even prompting, so without it
# the daemon could never be granted Automation and sending could not move off the
# CLI (§7).
info_plist() {
	cat <<-EOF
		<?xml version="1.0" encoding="UTF-8"?>
		<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
		<plist version="1.0">
		<dict>
		  <key>CFBundleIdentifier</key><string>$IDENTIFIER</string>
		  <key>CFBundleName</key><string>msgd</string>
		  <key>CFBundleExecutable</key><string>msgd</string>
		  <key>CFBundleIconFile</key><string>msgd</string>
		  <key>CFBundlePackageType</key><string>APPL</string>
		  <key>CFBundleShortVersionString</key><string>$version</string>
		  <key>LSBackgroundOnly</key><true/>
		  <key>NSAppleEventsUsageDescription</key><string>msg sends and reads messages through Messages.</string>
		  <key>NSContactsUsageDescription</key><string>msg shows contact names instead of phone numbers.</string>
		</dict>
		</plist>
	EOF
}

(cd "$root" && cargo build --release)

# A stale _CodeSignature from a previous build makes the bundle fail to validate,
# so start from nothing rather than writing over it.
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$root/target/release/msgd" "$app/Contents/MacOS/msgd"
info_plist >"$app/Contents/Info.plist"

# Committed rather than generated, so the build needs nothing but a copy.
# scripts/build-icon.sh redraws it from assets/msgd.svg, and is not run here.
icon="$root/assets/msgd.icns"
[ -f "$icon" ] || die "no icon at $icon
Redraw it with ./scripts/build-icon.sh."
cp "$icon" "$app/Contents/Resources/msgd.icns"

# `msg` sits beside the bundle, which is where `msg daemon install` looks for it.
cp "$root/target/release/msg" "$out/msg"

# Signing comes last: everything above invalidates a signature. Signing the
# bundle rather than the executable is what produces _CodeSignature, and
# --identifier fixes the name the grant is keyed to whatever the file is called.
codesign --force --sign "$identity" --identifier "$IDENTIFIER" "$app"

# The bundle is the whole point, so check that codesign saw one. Signing a
# directory that macOS does not recognise as a bundle succeeds quietly and
# produces exactly the path-keyed grant this layout exists to avoid.
report=$(codesign -dv "$app" 2>&1)
case $report in
*"Format=app bundle"*) ;;
*) die "codesign does not see an app bundle:
$report" ;;
esac
case $report in
*"Identifier=$IDENTIFIER"*) ;;
*) die "signed with the wrong identifier:
$report" ;;
esac

size=$(( $(stat -f%z "$app/Contents/MacOS/msgd") / 1024 ))
echo "built $app (${size}KB, signed $identity)"
echo "built $out/msg"
if [ "$identity" = "-" ]; then
	echo "Signed ad-hoc: every rebuild changes the cdhash, so Full Disk Access has to be"
	echo "granted again. Unset MSG_SIGN_IDENTITY to sign with a stable certificate instead."
fi
