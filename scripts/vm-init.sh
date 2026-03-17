#!/bin/sh
# Minimal init script for Firecracker guest VMs.
# Mounts essential filesystems, then execs the rustbox-agent.

mount -t proc proc /proc
mount -t sysfs sysfs /sys
mount -t devtmpfs devtmpfs /dev

exec /usr/bin/rustbox-agent
