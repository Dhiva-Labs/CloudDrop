#!/usr/bin/env bash
# Rebuilds packaging/vendor/crates-<version>.tar.xz and SHA256SUMS.
# Run from the repository root after any Cargo.lock change. Needs network.
#
# Every crate in Cargo.lock is vendored (Cargo resolves the whole lockfile),
# but crates the Linux build of clouddrop + clouddrop-gui doesn't compile
# (Windows/macOS-only crates, test-only dev-dependencies) are replaced by
# stubs: their Cargo.toml, an empty src/lib.rs and a .cargo-checksum.json with
# no files but the original package checksum. One tarball serves both Linux
# architectures (amd64 and arm64), so a crate is kept when either one builds it.
set -euo pipefail
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
out=packaging/vendor/crates-$version.tar.xz
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

cargo vendor --locked --versioned-dirs "$work/vendor" >/dev/null
for triple in x86_64-unknown-linux-gnu aarch64-unknown-linux-gnu; do
    cargo tree --locked --target "$triple" -e normal,build \
        -p clouddrop -p clouddrop-gui -p clouddrop-tray --prefix none --format '{p}' \
        | awk '{print $1"-"substr($2,2)}'
done | sort -u > "$work/keep"

python3 - "$work/vendor" "$work/keep" <<'PY'
import json, os, shutil, sys
vendor, keep = sys.argv[1], set(open(sys.argv[2]).read().split())
stubbed = 0
for name in sorted(os.listdir(vendor)):
    if name in keep:
        continue
    d = os.path.join(vendor, name)
    checksum = json.load(open(os.path.join(d, ".cargo-checksum.json")))["package"]
    manifest = open(os.path.join(d, "Cargo.toml")).read()
    shutil.rmtree(d)
    os.makedirs(os.path.join(d, "src"))
    open(os.path.join(d, "Cargo.toml"), "w").write(manifest)
    open(os.path.join(d, "src", "lib.rs"), "w").write("")
    json.dump({"files": {}, "package": checksum}, open(os.path.join(d, ".cargo-checksum.json"), "w"))
    stubbed += 1
print(f"kept {len(keep)} crates, stubbed {stubbed}")
PY

tar -C "$work" --sort=name --owner=0 --group=0 --numeric-owner --mtime=2026-01-01 \
    -cJf "$out" vendor
(cd packaging/vendor && sha256sum rustc-*.tar.xz cargo-*.tar.xz rust-std-*.tar.xz \
    "crates-$version.tar.xz" > SHA256SUMS)
echo "wrote $out ($(du -h "$out" | cut -f1))"
