#!/bin/sh
# Before removing the omnimem package (.deb prerm, .rpm %preun): stop and
# disable the service, but not on an upgrade, where postinst restarts it.
# dpkg passes "remove"; rpm passes 0 when the last copy goes.
set -e

case "${1:-}" in
    remove | 0)
        if [ -d /run/systemd/system ]; then
            systemctl disable --now omnimem.service >/dev/null 2>&1 || true
        fi
        ;;
esac
exit 0
