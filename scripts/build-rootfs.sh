#!/usr/bin/env bash
# Build an ext4 rootfs image from a Docker image.
#
# Usage: build-rootfs.sh <docker-image> <output.ext4> [size_mb]
#
# Requires: docker, e2fsprogs >= 1.43 (for mkfs.ext4 -d)

set -euo pipefail

DOCKER_IMAGE="${1:?Usage: build-rootfs.sh <docker-image> <output.ext4> [size_mb]}"
OUTPUT="${2:?Usage: build-rootfs.sh <docker-image> <output.ext4> [size_mb]}"
SIZE_MB="${3:-2048}"

TMPDIR="$(mktemp -d)"
CONTAINER_ID=""

cleanup() {
    [ -n "$CONTAINER_ID" ] && docker rm -f "$CONTAINER_ID" >/dev/null 2>&1 || true
    rm -rf "$TMPDIR"
}
trap cleanup EXIT

echo "==> Creating container from ${DOCKER_IMAGE}"
CONTAINER_ID=$(docker create "$DOCKER_IMAGE")

echo "==> Exporting filesystem to ${TMPDIR}"
docker export "$CONTAINER_ID" | tar -xf - -C "$TMPDIR"

echo "==> Creating ext4 image (${SIZE_MB} MiB) at ${OUTPUT}"
dd if=/dev/zero of="$OUTPUT" bs=1M count="$SIZE_MB" status=none
mkfs.ext4 -d "$TMPDIR" -L rootfs "$OUTPUT"

echo "==> Validating image"
e2fsck -fn "$OUTPUT"

echo "==> Done: ${OUTPUT}"
