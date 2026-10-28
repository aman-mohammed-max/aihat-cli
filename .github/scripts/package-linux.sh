#!/usr/bin/env bash
set -euo pipefail

target="$1"
nfpm_arch="$2"
formats="$3"
linux_name="$4"
debian_arch="$5"
rpm_arch="$6"
archlinux_arch="$7"
apk_arch="$8"

cross build --locked --release --target "$target"
mkdir -p dist packaging
cp "target/$target/release/aihat" packaging/aihat
tar -czf "dist/UniversalMemory-linux-${linux_name}.tar.gz" -C packaging aihat

PACKAGE_VERSION="$(awk -F '"' '/^version = / { print $2; exit }' Cargo.toml)"
if [[ "${GITHUB_REF_TYPE:-}" == "tag" ]]; then
  PACKAGE_VERSION="${GITHUB_REF_NAME#v}"
fi
export PACKAGE_VERSION NFPM_ARCH="$nfpm_arch"

for format in $formats; do
  case "$format" in
    deb)
      output="dist/UniversalMemory-linux-${debian_arch}.deb"
      ;;
    rpm)
      output="dist/UniversalMemory-fedora-${rpm_arch}.rpm"
      ;;
    archlinux)
      output="dist/UniversalMemory-arch-${archlinux_arch}.pkg.tar.zst"
      ;;
    apk)
      output="dist/UniversalMemory-alpine-${apk_arch}.apk"
      ;;
    *)
      echo "Unsupported package format: $format" >&2
      exit 1
      ;;
  esac

  nfpm package --config packaging/nfpm.yaml --packager "$format" --target "$output"
done

rm packaging/aihat
