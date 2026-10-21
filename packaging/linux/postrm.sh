#!/bin/sh
# After removing the omnimem package (.deb postrm, .rpm %postun). The data
# in /var/lib/omnimem and the omnimem user stay: memories aren't something a
# package should delete. Remove them by hand if you mean to.
set -e

case "${1:-}" in
    remove | purge | 0)
        if [ -d /run/systemd/system ]; then
            systemctl daemon-reload || true
        fi
        if [ -d /var/lib/omnimem ]; then
            echo "Your memories are still in /var/lib/omnimem."
        fi
        ;;
esac
exit 0
