#!/bin/sh
# Packs a release build into dist/tilog-<version>-<target>.tar.gz, with a checksum next to it.
#
#   scripts/package.sh <target> [binary]
#
# <target> only names the archive (for example x86_64-unknown-linux-musl). <binary> is the built
# program; it defaults to target/<target>/release/tilog, or target/release/tilog when that does not
# exist (a build without --target). The archive holds the program, the manual page and the
# documents, in a directory of the same name.
set -eu

target=${1:?usage: scripts/package.sh <target> [binary]}
binary=${2:-target/$target/release/tilog}
[ -f "$binary" ] || binary=target/release/tilog
[ -f "$binary" ] || { echo "no built program: $binary" >&2; exit 1; }

version=$(cargo pkgid | sed 's/.*[#@]//')
name=tilog-$version-$target
stage=dist/$name

rm -rf "$stage"
mkdir -p "$stage/man"
cp "$binary" "$stage/tilog"
cp man/tilog.1 "$stage/man/"
cp README.md LICENSE CHANGELOG.md "$stage/"

tar -C dist -czf "dist/$name.tar.gz" "$name"
rm -rf "$stage"
(cd dist && shasum -a 256 "$name.tar.gz" > "$name.tar.gz.sha256")
echo "dist/$name.tar.gz"
