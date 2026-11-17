#!/usr/bin/env bash
# Snapshot the current working files, including uncommitted port changes, then use makepkg.
set -euo pipefail

root=$(realpath "$(dirname "${BASH_SOURCE[0]}")/..")
prepare_only=false
if [[ ${1:-} == --prepare-only ]]; then
    [[ $# == 2 ]] || { printf 'Usage: bash tools/arch.sh --prepare-only EMPTY_DIRECTORY\n' >&2; exit 2; }
    prepare_only=true
    destination=$(realpath -m "$2")
    shift 2
else
    command -v makepkg >/dev/null || { printf 'Native package building requires Arch makepkg. Use --prepare-only DIRECTORY to create transferable sources here.\n' >&2; exit 1; }
    [[ $EUID != 0 ]] || { printf 'Run makepkg as your regular user, not root.\n' >&2; exit 1; }
    destination=$(mktemp -d "${TMPDIR:-/tmp}/amber-arch.XXXXXXXX")
fi

case "$destination/" in
    "$root/"*) printf 'Use a staging directory outside the checkout.\n' >&2; exit 2 ;;
esac
mkdir -p "$destination"
shopt -s nullglob dotglob
existing=("$destination"/*)
shopt -u nullglob dotglob
[[ ${#existing[@]} == 0 ]] || { printf 'Refusing to overwrite a nonempty staging directory: %s\n' "$destination" >&2; exit 2; }

version=$(node -e 'const p = require(process.argv[1]); if (!/^\d+\.\d+\.\d+(?:[-+][A-Za-z0-9.-]+)?$/.test(p.version)) process.exit(1); console.log(p.version.replaceAll("-", "_"))' "$root/package.json")
epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --format=%ct 2>/dev/null || printf '0')}
[[ $epoch =~ ^[0-9]+$ ]] || { printf 'SOURCE_DATE_EPOCH must be a nonnegative integer.\n' >&2; exit 2; }
name="amber-dictation-$version"

# An explicit source allowlist excludes repository metadata and unrelated local files.
# Never dereference source symlinks or include generated outputs/signing material.
sources=(.cargo Cargo.toml Cargo.lock package.json package-lock.json LICENSE README.md
         tsconfig.json vite.config.ts settings.html bubble.html crates server src src-tauri tools packaging)
tar --create --file "$destination/$name.tar.gz" --use-compress-program='gzip -n' \
    --sort=name --mtime="@$epoch" --owner=0 --group=0 --numeric-owner --mode='u+rwX,go+rX,go-w' \
    --exclude=.git --exclude=node_modules --exclude=target --exclude=dist --exclude='src-tauri/gen' \
    --exclude='.env' --exclude='.env.*' --exclude='*.key' --exclude='*.password' --exclude='*.pem' \
    --exclude='*.p12' --exclude='*.pfx' --exclude='*.pkg.tar.*' \
    --transform="flags=rh;s,^,$name/," --directory="$root" "${sources[@]}"
digest=$(sha256sum "$destination/$name.tar.gz")
digest=${digest%% *}
recipe=$(<"$root/packaging/arch/PKGBUILD.in")
recipe=${recipe//@VERSION@/$version}
recipe=${recipe//@SHA256@/$digest}
printf '%s\n' "$recipe" >"$destination/PKGBUILD"
printf '%s\n' "$epoch" >"$destination/SOURCE_DATE_EPOCH"
printf 'Prepared current-checkout sources in %s\nSHA256 %s\n' "$destination" "$digest"

if $prepare_only; then
    exit 0
fi

# Keep the staging directory and build log/cache available if makepkg fails.
export PKGDEST="${PKGDEST:-$root/target/arch}"
mkdir -p "$PKGDEST"
export SOURCE_DATE_EPOCH=$epoch
(
    cd "$destination"
    makepkg --syncdeps --check "$@"
)
printf 'Packages are in %s\nBuild sources/cache are in %s\n' "$PKGDEST" "$destination"
