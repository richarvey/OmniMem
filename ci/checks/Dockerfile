# Coverage and the dependency audit: the pinned Rust (scripts pass the version
# from ci/desktop/Dockerfile, which holds the one pin), LLVM's coverage tools,
# and cargo-llvm-cov and cargo-audit at fixed versions, built --locked once
# per runner. jq reads the coverage totals for the badge.
ARG RUST_VERSION=1
FROM rust:${RUST_VERSION}-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends jq \
    && rm -rf /var/lib/apt/lists/*
RUN rustup component add llvm-tools-preview \
    && cargo install --locked cargo-llvm-cov@0.9.1 cargo-audit@0.22.2 \
    && rm -rf /usr/local/cargo/registry /usr/local/cargo/git
