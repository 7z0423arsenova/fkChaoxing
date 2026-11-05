#!/bin/sh
set -eu

cd "$(dirname "$0")/.."
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform=linux-amd64 ;;
  Linux-aarch64) platform=linux-arm64 ;;
  Darwin-x86_64) platform=darwin-amd64 ;;
  Darwin-arm64) platform=darwin-arm64 ;;
  *) echo "Unsupported host; build the Rust core for this platform manually." >&2; exit 1 ;;
esac

cargo build --release --locked
mkdir -p "desktop/resources/$platform/bin"
cp target/release/fkChaoxing "desktop/resources/$platform/bin/fkChaoxing-core"
cd desktop
go run github.com/egoist/mygo/cmd/mygo build
