#!/usr/bin/env bash
set -euo pipefail

version="$1"
platform="$2"
case "$platform" in
    macos-arm64) architecture=arm64 ;;
    macos-x64) architecture=x86_64 ;;
    *) echo "Unknown macOS platform: $platform" >&2; exit 1 ;;
esac
test "$(lipo -archs target/release/SenseSim)" = "$architecture"

work="$(mktemp -d)"
trap 'security delete-keychain "$work/signing.keychain-db" 2>/dev/null || true; rm -rf -- "$work"' EXIT
app="$work/Sense Sim.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist
install -m 755 target/release/SenseSim "$app/Contents/MacOS/SenseSim"
iconset="$work/AppIcon.iconset"
mkdir "$iconset"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" assets/logo_1024.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" assets/logo_1024.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key><string>com.nextwaves.sensesim</string>
    <key>CFBundleExecutable</key><string>SenseSim</string>
    <key>CFBundleName</key><string>Sense Sim</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>CFBundleIconFile</key><string>AppIcon</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
plutil -lint "$app/Contents/Info.plist"

package="dist/SenseSim-$version-$platform.pkg"
if [[ -n "${MACOS_CERTIFICATE:-}" ]]; then
    keychain="$work/signing.keychain-db"
    password="$(openssl rand -hex 24)"
    printf '%s' "$MACOS_CERTIFICATE" | base64 -D > "$work/certificates.p12"
    security create-keychain -p "$password" "$keychain"
    security unlock-keychain -p "$password" "$keychain"
    security import "$work/certificates.p12" -k "$keychain" -P "${MACOS_CERTIFICATE_PASSWORD:?}" -T /usr/bin/codesign -T /usr/bin/productbuild
    security set-key-partition-list -S apple-tool:,apple: -s -k "$password" "$keychain"
    codesign --keychain "$keychain" --sign "${MACOS_APP_IDENTITY:?}" --options runtime --timestamp "$app"
    productbuild --keychain "$keychain" --sign "${MACOS_INSTALLER_IDENTITY:?}" --component "$app" /Applications "$package"
    printf '%s' "${MACOS_NOTARY_KEY:?}" > "$work/notary.p8"
    chmod 600 "$work/notary.p8"
    xcrun notarytool submit "$package" --key "$work/notary.p8" --key-id "${MACOS_NOTARY_KEY_ID:?}" --issuer "${MACOS_NOTARY_ISSUER:?}" --wait
    xcrun stapler staple "$package"
    xcrun stapler validate "$package"
else
    if [[ "${GITHUB_REF:-}" == refs/tags/v* ]]; then
        echo "::warning::macOS installer is unsigned. Configure Developer ID credentials for Gatekeeper-trusted releases."
    fi
    codesign --sign - "$app"
    productbuild --component "$app" /Applications "$package"
fi
codesign --verify --strict "$app"
