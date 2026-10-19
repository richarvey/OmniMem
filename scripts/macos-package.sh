#!/usr/bin/env bash
# Build OmniMem for macOS and package it for installing:
#
#   OmniMem-<version>-arm64.dmg           OmniMem.app, a menu bar app with no
#                                         Dock icon, beside an Applications link
#   omnimem-<version>-macos-arm64.tar.gz  the same binary on its own, for
#                                         `omnimem serve` and `omnimem hook`
#
# each with a .sha256 beside it, in $OMNIMEM_DIST (default dist/macos). The
# version is the workspace version in Cargo.toml.
#
# Apple silicon only: ort has no prebuilt ONNX Runtime for x86_64-apple-darwin,
# so there is no Intel slice to make a universal binary with. Its library is
# built for macOS 13.4, which sets the minimum here.
#
# Signing: with MACOS_SIGN_IDENTITY set to a "Developer ID Application"
# identity in the keychain, the app and binary are signed with the hardened
# runtime, and with MACOS_NOTARY_KEY (an App Store Connect API key, .p8),
# MACOS_NOTARY_KEY_ID and MACOS_NOTARY_ISSUER as well the DMG is notarised and
# stapled. Without an identity everything is ad-hoc signed: it runs, but
# Gatekeeper stops the first open of a downloaded copy until it's allowed
# under System Settings > Privacy & Security.
#
# Needs the Xcode command line tools and a Rust toolchain with the
# aarch64-apple-darwin target.
set -euo pipefail

repo="$(cd "$(dirname "$0")/.." && pwd)"
dist="${OMNIMEM_DIST:-$repo/dist/macos}"
target=aarch64-apple-darwin
target_dir="${CARGO_TARGET_DIR:-$repo/target}"
version="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$repo/Cargo.toml" | head -n 1)"
# Bundle versions are dotted numbers only: 7.0.0-dev becomes 7.0.0.
bundle_version="${version%%[-+]*}"
export MACOSX_DEPLOYMENT_TARGET=13.4

if [ "$(uname -s)" != Darwin ]; then
    echo "macos-package.sh builds on macOS only" >&2
    exit 1
fi
if [ ! -d "$(rustc --print target-libdir --target "$target")" ]; then
    echo "The Rust toolchain on PATH has no $target standard library:" >&2
    echo "rustup target add $target" >&2
    exit 1
fi

cargo build --manifest-path "$repo/Cargo.toml" --release --locked \
    -p omnimem --features desktop --target "$target"
binary="$target_dir/$target/release/omnimem"
"$binary" --help >/dev/null

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

app="$work/OmniMem.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$binary" "$app/Contents/MacOS/omnimem"
# The app icon is drawn by the same code as the menu bar icon; the example
# writes it at every size iconutil wants.
cargo run --manifest-path "$repo/Cargo.toml" --release --locked --quiet \
    -p omnimem-desktop --example app-icon --target "$target" -- "$work/OmniMem.iconset"
iconutil --convert icns --output "$app/Contents/Resources/OmniMem.icns" "$work/OmniMem.iconset"
# LSUIElement keeps the Dock icon from flashing up before the app sets its
# accessory activation policy.
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleDisplayName</key>
    <string>OmniMem</string>
    <key>CFBundleExecutable</key>
    <string>omnimem</string>
    <key>CFBundleIconFile</key>
    <string>OmniMem</string>
    <key>CFBundleIdentifier</key>
    <string>com.squarecows.OmniMem</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>OmniMem</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$bundle_version</string>
    <key>CFBundleVersion</key>
    <string>$bundle_version</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key>
    <string>$MACOSX_DEPLOYMENT_TARGET</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist" >/dev/null

cli="$work/omnimem"
cp "$binary" "$cli"
if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    sign=(codesign --force --options runtime --timestamp --sign "$MACOS_SIGN_IDENTITY")
else
    sign=(codesign --force --sign -)
fi
"${sign[@]}" "$app"
"${sign[@]}" "$cli"
codesign --verify --strict "$app"

mkdir -p "$dist"
dmg="$dist/OmniMem-$version-arm64.dmg"
tarball="$dist/omnimem-$version-macos-arm64.tar.gz"

stage="$work/dmg"
mkdir "$stage"
ditto "$app" "$stage/OmniMem.app"
ln -s /Applications "$stage/Applications"
# hdiutil now and then fails with "Resource busy" while something indexes the
# new volume; a second go normally works.
for attempt in 1 2 3; do
    if hdiutil create -volname "OmniMem" -srcfolder "$stage" -fs HFS+ \
        -format UDZO -ov "$dmg" >/dev/null; then
        break
    fi
    [ "$attempt" = 3 ] && exit 1
    sleep 5
done

if [ -n "${MACOS_SIGN_IDENTITY:-}" ]; then
    codesign --force --timestamp --sign "$MACOS_SIGN_IDENTITY" "$dmg"
    if [ -n "${MACOS_NOTARY_KEY:-}" ]; then
        xcrun notarytool submit "$dmg" --wait \
            --key "$MACOS_NOTARY_KEY" \
            --key-id "$MACOS_NOTARY_KEY_ID" \
            --issuer "$MACOS_NOTARY_ISSUER"
        # Fails unless notarisation was accepted, so it doubles as the check.
        xcrun stapler staple "$dmg"
    fi
fi

cp "$repo/LICENSE" "$work/LICENSE"
tar -czf "$tarball" -C "$work" omnimem LICENSE

cd "$dist"
for file in "$(basename "$dmg")" "$(basename "$tarball")"; do
    shasum -a 256 "$file" > "$file.sha256"
done
echo "Packaged OmniMem $version for macOS (arm64) in $dist:"
ls -l "$dist"
