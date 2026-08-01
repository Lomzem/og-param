FROM rust:1.94.0-bookworm AS artifacts

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update && \
    apt-get install --no-install-recommends -y \
        gcc-mingw-w64-x86-64 \
        musl-tools && \
    rm -rf /var/lib/apt/lists/* && \
    rustup target add x86_64-unknown-linux-musl x86_64-pc-windows-gnu

WORKDIR /workspace
COPY Cargo.toml Cargo.lock ./
RUN mkdir src && \
    printf 'fn main() {}\n' > src/main.rs && \
    cargo build --locked --release --target x86_64-unknown-linux-musl && \
    cargo build --locked --release --target x86_64-pc-windows-gnu

COPY src src
COPY scripts/artifact-entrypoint.sh /usr/local/bin/og-param-artifacts

ENV CARGO_NET_OFFLINE=true
ENTRYPOINT ["og-param-artifacts"]
