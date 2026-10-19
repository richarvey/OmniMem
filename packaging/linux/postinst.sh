#!/bin/sh
# After installing the omnimem package (.deb postinst, .rpm %post): the
# service's user and data folder, a root-only settings file, and the service
# enabled and started where systemd runs. dpkg passes "configure"; rpm passes
# the number of installed copies.
set -e

case "${1:-}" in
    configure | [0-9]*) ;;
    *) exit 0 ;;
esac

if ! id omnimem >/dev/null 2>&1; then
    useradd --system --user-group --home-dir /var/lib/omnimem --no-create-home \
        --shell /usr/sbin/nologin --comment "OmniMem" omnimem
fi
install -d -o omnimem -g omnimem -m 0750 /var/lib/omnimem
# It can hold an API key and tokens; systemd reads it as root.
chown root:root /etc/omnimem/omnimem.env
chmod 0600 /etc/omnimem/omnimem.env

if [ -d /run/systemd/system ]; then
    systemctl daemon-reload
    systemctl enable omnimem.service >/dev/null 2>&1 || true
    systemctl restart omnimem.service || true
    echo "The omnimem service is running. Settings: /etc/omnimem/omnimem.env; log: journalctl -u omnimem"
fi
exit 0
