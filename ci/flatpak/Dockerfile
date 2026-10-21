# Build environment for the Flatpak: flatpak-builder, plus Xvfb and a D-Bus
# session to smoke-test the bundle in its sandbox. The GNOME SDK and the Rust
# extension are installed into the cache folder by scripts/flatpak-package.sh,
# not into the image, so they download once per runner and update in place.
# bubblewrap needs user namespaces, so the container runs with --privileged.
FROM debian:13

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        appstream \
        bzip2 \
        ca-certificates \
        debugedit \
        desktop-file-utils \
        elfutils \
        flatpak \
        flatpak-builder \
        git \
        patch \
        unzip \
        xz-utils \
        xvfb \
        xauth \
        dbus \
        dbus-x11 \
    && rm -rf /var/lib/apt/lists/*
