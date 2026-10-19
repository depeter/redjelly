#!/usr/bin/env bash
# Publishes a release that installed copies of redjelly update themselves to
# (app/src/update.rs): bumps the patch version (or sets the one given),
# builds the Windows installer, signs it, commits and tags, then — after
# asking — pushes and creates the GitHub release.
#
#   LLVM_MINGW=... MPV_DEV=... MAKENSIS=... NSISDIR=... scripts/release.sh [x.y.z]
#
# Build variables: see scripts/build-windows.sh. The signing key is
# ~/.config/redjelly/release-signing.key (override with REDJELLY_SIGNING_KEY);
# its public half is PUBLIC_KEY in app/src/update.rs.
set -euo pipefail

die() { echo "release: $*" >&2; exit 1; }

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"
key="${REDJELLY_SIGNING_KEY:-$HOME/.config/redjelly/release-signing.key}"

: "${MAKENSIS:?set MAKENSIS (and the other scripts/build-windows.sh variables)}"
[ -f "$key" ] || die "no signing key at $key"
command -v gh >/dev/null || die "gh (GitHub CLI) not found"
[ "$(git branch --show-current)" = main ] || die "not on main"
[ -z "$(git status --porcelain)" ] || die "uncommitted changes; commit or stash them first"

# The app must trust this key, or the release can never be installed.
pub=$(openssl pkey -in "$key" -pubout -outform DER | tail -c 32 | od -An -tx1 | tr -d ' \n')
grep -q "$pub" app/src/update.rs || die "signing key doesn't match PUBLIC_KEY in app/src/update.rs"

old=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
if [ $# -ge 1 ]; then
  new="$1"
else
  IFS=. read -r major minor patch <<<"$old"
  new="$major.$minor.$((patch + 1))"
fi
[[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "bad version '$new'"
git rev-parse -q --verify "refs/tags/v$new" >/dev/null && die "tag v$new already exists"
echo "release: $old -> $new"

prev_tag=$(git describe --tags --abbrev=0 --match 'v*' 2>/dev/null || true)
notes="$root/dist/notes-$new.md"
mkdir -p dist
git log --no-merges --format='- %s' ${prev_tag:+"$prev_tag"..}HEAD | grep -v '^- Release ' | head -50 >"$notes" || true
[ -s "$notes" ] || echo "- Maintenance release" >"$notes"

sed -i "0,/^version = \"$old\"/s//version = \"$new\"/" Cargo.toml
cargo update -w --quiet

"$root/scripts/build-windows.sh"

exe="dist/redjelly-install-$new.exe"
[ -f "$exe" ] || die "build didn't produce $exe"
openssl pkeyutl -sign -rawin -inkey "$key" -in "$exe" -out "$exe.sig"
openssl pkey -in "$key" -pubout -out "dist/release-signing.pub"
openssl pkeyutl -verify -rawin -pubin -inkey "dist/release-signing.pub" -in "$exe" -sigfile "$exe.sig" >/dev/null \
  || die "signature check failed"

git commit -q -am "Release $new"
git tag -a "v$new" -m "redjelly $new"
echo
echo "Release notes:"
cat "$notes"
echo
read -rp "Push main + v$new and publish the GitHub release? [y/N] " answer
if [ "$answer" != y ] && [ "$answer" != Y ]; then
  echo "Not published. Undo with: git tag -d v$new && git reset --hard HEAD~1"
  exit 0
fi
git push origin main "v$new"
gh release create "v$new" --title "redjelly $new" --notes-file "$notes" \
  "$exe" "$exe.sig" "dist/redjelly-install-$new.zip"
echo "published redjelly $new"
