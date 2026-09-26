#!/bin/sh
# Puts the kanbr binary at target/release/kanbr. Herdr runs this as the
# plugin's build step on `herdr plugin install`; it is also safe to run by hand.
#
# 1. Download the prebuilt binary for this version and platform from the
#    GitHub release and verify its SHA-256 (no Rust toolchain needed).
# 2. Otherwise build from source with cargo.
#
# KANBR_BUILD_FROM_SOURCE=1 skips the download.
set -eu

cd "$(dirname "$0")/.."
repo="alvinleyble/kanbr"
dest="target/release/kanbr"
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' herdr-plugin.toml | head -n 1)

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *) target="" ;;
esac

sha256() {
  if command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d ' ' -f 1
  else
    sha256sum "$1" | cut -d ' ' -f 1
  fi
}

download() {
  [ -n "$target" ] && [ -n "$version" ] || return 1
  command -v curl >/dev/null 2>&1 || return 1
  asset="kanbr-$target.tar.gz"
  url="https://github.com/$repo/releases/download/v$version/$asset"
  tmp=$(mktemp -d)
  if curl -fsSL --retry 2 -o "$tmp/$asset" "$url" &&
    curl -fsSL --retry 2 -o "$tmp/$asset.sha256" "$url.sha256"; then
    want=$(cut -d ' ' -f 1 <"$tmp/$asset.sha256")
    got=$(sha256 "$tmp/$asset")
    if [ -n "$want" ] && [ "$want" = "$got" ] && tar -xzf "$tmp/$asset" -C "$tmp" kanbr &&
      "$tmp/kanbr" --version >/dev/null 2>&1; then
      mkdir -p "$(dirname "$dest")"
      mv "$tmp/kanbr" "$dest"
      rm -rf "$tmp"
      return 0
    fi
    echo "kanbr: release download for $target failed verification; building from source" >&2
  fi
  rm -rf "$tmp"
  return 1
}

if [ "${KANBR_BUILD_FROM_SOURCE:-0}" != 1 ] && download; then
  echo "kanbr: installed prebuilt v$version for $target"
  exit 0
fi

cargo=$(command -v cargo 2>/dev/null || true)
[ -n "$cargo" ] || [ ! -x "$HOME/.cargo/bin/cargo" ] || cargo="$HOME/.cargo/bin/cargo"
if [ -z "$cargo" ]; then
  echo "kanbr: no prebuilt binary for v$version on ${target:-this platform}, and cargo was not found." >&2
  echo "kanbr: install Rust from https://rustup.rs, then reinstall the plugin." >&2
  exit 1
fi
"$cargo" build --release --locked
echo "kanbr: built v$version from source"
