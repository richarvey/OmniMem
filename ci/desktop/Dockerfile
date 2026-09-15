# Build and smoke-test environment for omnimem-desktop on Linux: the GTK,
# WebKitGTK and AppIndicator development libraries the tray and settings
# window link against, plus Xvfb and a D-Bus session for headless checks.
# Used by scripts/desktop-check.sh so none of this is installed on the host.
# Pinned to the toolchain the host builds with, so a newer clippy can't fail
# a check here that passes there. Move it when the workspace moves.
FROM rust:1.96-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        libwebkit2gtk-4.1-dev \
        libgtk-3-dev \
        libayatana-appindicator3-dev \
        libxdo-dev \
        xvfb \
        xauth \
        dbus \
        dbus-x11 \
        at-spi2-core \
    && rm -rf /var/lib/apt/lists/*

RUN rustup component add clippy rustfmt
