#!/bin/sh
# Install OmniMem's server from this folder, on a Linux machine without the
# .deb or .rpm: the binary to /usr/local/bin, its settings file
# /etc/omnimem/omnimem.env (yours is kept if it's already there), its data
# folder /var/lib/omnimem and, with systemd, the omnimem service, started.
#
#   sudo ./install.sh
#
# Needs glibc 2.38 or newer (Debian 13, Ubuntu 24.04, Fedora 39 and later).
set -eu

here="$(cd "$(dirname "$0")" && pwd)"
if [ "$(id -u)" -ne 0 ]; then
    echo "Run it as root: sudo $0" >&2
    exit 1
fi

install -m 0755 "$here/omnimem" /usr/local/bin/omnimem
if ! id omnimem >/dev/null 2>&1; then
    useradd --system --user-group --home-dir /var/lib/omnimem --no-create-home \
        --shell /usr/sbin/nologin --comment "OmniMem" omnimem
fi
install -d -o omnimem -g omnimem -m 0750 /var/lib/omnimem
install -d -m 0755 /etc/omnimem
if [ -e /etc/omnimem/omnimem.env ]; then
    echo "Kept your /etc/omnimem/omnimem.env"
else
    install -m 0640 "$here/omnimem.env" /etc/omnimem/omnimem.env
fi
# Read by the service as the omnimem user, and by commands run as it.
chown root:omnimem /etc/omnimem/omnimem.env
chmod 0640 /etc/omnimem/omnimem.env
# Before 7.0.4 the service unit set these two, and a settings file kept from
# then doesn't have them. Without HF_HOME the model would be downloaded again.
add_if_missing() {
    if ! grep -q "^[[:space:]]*$1=" /etc/omnimem/omnimem.env; then
        {
            echo
            echo "# Added on upgrade: the service unit used to set this."
            echo "$1=$2"
        } >>/etc/omnimem/omnimem.env
    fi
}
add_if_missing OMNIMEM_DB /var/lib/omnimem/omnimem.db
add_if_missing HF_HOME /var/lib/omnimem/huggingface

if [ -d /run/systemd/system ]; then
    sed 's|/usr/bin/omnimem|/usr/local/bin/omnimem|' "$here/omnimem.service" \
        >/etc/systemd/system/omnimem.service
    systemctl daemon-reload
    systemctl enable omnimem.service >/dev/null 2>&1
    systemctl restart omnimem.service
    echo "OmniMem is running at http://127.0.0.1:8765/mcp"
    echo "Settings: /etc/omnimem/omnimem.env, then: sudo systemctl restart omnimem"
    echo "Log: journalctl -u omnimem -f"
else
    echo "Installed. Without systemd, start it as the omnimem user; it reads /etc/omnimem/omnimem.env:"
    echo "  sudo -u omnimem /usr/local/bin/omnimem serve"
fi
