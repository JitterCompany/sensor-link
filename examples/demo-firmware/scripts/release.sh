#!/usr/bin/env bash
# Build, sign and package a demo firmware release into examples/demo-firmware/dist/:
#
#   bootloader-v<version>-<rev>.elf        bootloader, for provisioning
#   demo-v<version>-<rev>.elf              signed application, for provisioning
#   demo-v<version>-<rev>-<suffix>.bin.sig.b64
#                                          signed update image for the server's
#                                          firmware directory (base64url)
#   provision.toml                         profile for sensor-link-provision
#
# Usage: scripts/release.sh <version> [suffix]     (suffix defaults to "dev")
#
# The signing key is $DEMO_SIGNING_KEY, default keys/app-signing-key.pem. It
# must match the public key the bootloader was built with (DEMO_APP_PUBKEY).

set -euo pipefail

WORKSPACE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPO="$(cd "$WORKSPACE/../.." && pwd)"
VERSION="${1:?usage: $0 <version> [suffix]}"
SUFFIX="${2:-dev}"
KEY="${DEMO_SIGNING_KEY:-$WORKSPACE/keys/app-signing-key.pem}"
GIT_REV="$(git -C "$REPO" rev-parse --short=8 HEAD)"
NAME="v${VERSION}-${GIT_REV}"

cd "$WORKSPACE"
SYSROOT="$(rustc --print sysroot)"
OBJCOPY="$(find "$SYSROOT" -name llvm-objcopy -type f | head -n 1)"
[ -x "$OBJCOPY" ] || { echo "llvm-objcopy not found: rustup component add llvm-tools" >&2; exit 1; }

# The firmware reports these, and the server compares them with the file name
export BUILD_VERSION="$VERSION"
export GIT_REV
cargo build --release -p demo-bootloader -p demo-firmware-app

OUT="$WORKSPACE/dist"
TARGET="$WORKSPACE/target/thumbv7em-none-eabihf/release"
mkdir -p "$OUT"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

cp "$TARGET/bootloader" "$OUT/bootloader-$NAME.elf"

"$OBJCOPY" -O binary "$TARGET/demo" "$WORK/demo.bin"
(cd "$REPO" && cargo run --quiet --release -p demo-firmware-signer -- "$WORK/demo.bin" "$KEY")

# The bootloader only boots a signed image, so the provisioning ELF gets the
# signed header too: the code is identical, only the header section differs.
"$OBJCOPY" --dump-section .flash_app_meta="$WORK/meta.bin" "$TARGET/demo"
head -c "$(wc -c < "$WORK/meta.bin")" "$WORK/demo.bin.sig" > "$WORK/meta.signed.bin"
"$OBJCOPY" --update-section .flash_app_meta="$WORK/meta.signed.bin" \
    "$TARGET/demo" "$OUT/demo-$NAME.elf"

# base64url, as the server's firmware directory expects
base64 < "$WORK/demo.bin.sig" | tr -d '\n' | tr '+/' '-_' \
    > "$OUT/demo-$NAME-$SUFFIX.bin.sig.b64"

cp "$WORKSPACE/provision.toml" "$OUT/"
ls -l "$OUT"
