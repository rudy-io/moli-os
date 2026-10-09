# Image de dev : toolchain Rust complète, aucune installation locale requise.
FROM rust:1-alpine
RUN apk add --no-cache musl-dev && rustup component add clippy rustfmt
ENV CARGO_TARGET_DIR=/target
WORKDIR /src
