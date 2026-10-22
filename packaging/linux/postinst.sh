#!/bin/sh
# After installing the omnimem package (.deb postinst, .rpm %post): the
# service's user and data folder, a settings file only root and the omnimem
# group can read, and the service enabled and started where systemd runs. dpkg passes "configure"; rpm passes
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
# It can hold an API key and tokens. The service reads it as the omnimem
# user, as do commands run with sudo -u omnimem.
chown root:omnimem /etc/omnimem/omnimem.env
chmod 0640 /etc/omnimem/omnimem.env
# Before 7.0.4 the service unit set these two, and a settings file kept from
# then doesn't have them. Without HF_HOME the model would be downloaded again.
# (No percent signs in here: this script is pasted into the rpm spec.)
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
    systemctl daemon-reload
    systemctl enable omnimem.service >/dev/null 2>&1 || true
    systemctl restart omnimem.service || true
    echo "The omnimem service is running. Settings: /etc/omnimem/omnimem.env; log: journalctl -u omnimem"
fi
exit 0
