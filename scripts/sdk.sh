#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
upstream=${TUTA_REPO:-$HOME/dev/clone/tutanota}
commit=$(sed -n 's/^commit=//p' sdk/BASE)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git -C "$upstream" archive "$commit" Cargo.toml LICENSE.txt tuta-sdk/rust \
  test/tests/api/worker/crypto/CompatibilityTestData.json | tar -x -C "$work"
for patch in sdk/patches/*.patch; do
  (cd "$work" && git apply --whitespace=nowarn "$OLDPWD/$patch") ||
    { echo "sdk: $patch does not apply" >&2; exit 1; }
done
rm -rf sdk/generated
chmod 755 "$work"
mv "$work" sdk/generated
trap - EXIT
