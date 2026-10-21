# Cross-build environment for the Windows app, on a Linux runner: the MSVC
# target through cargo-xwin, which fetches Microsoft's CRT and Windows SDK on
# first use (into the cache folder, so once per runner), clang and lld to
# compile and link for it, and wixl from msitools to make the MSI.
# The Rust version is ci/desktop/Dockerfile's, passed in by
# scripts/windows-package.sh, so every platform builds with the same one.
# windows-package.sh always passes the pinned version; the default only keeps
# a bare `docker build` working.
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        clang \
        lld \
        llvm \
        msitools \
        wixl \
    && rm -rf /var/lib/apt/lists/*

RUN rustup target add x86_64-pc-windows-msvc \
    && cargo install --locked cargo-xwin \
    && rm -rf "$CARGO_HOME/registry" "$CARGO_HOME/git"
