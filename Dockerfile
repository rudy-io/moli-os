# syntax=docker/dockerfile:1
# Release image: static musl binary with the dashboard embedded, on scratch.

FROM node:24-alpine AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
# The words (shared with the server): web/src/lib/i18n.svelte.js reads ../../../locales.
COPY locales/ /locales/
RUN npm run build

FROM rust:1-alpine AS build
RUN apk add --no-cache musl-dev
WORKDIR /src
COPY . .
COPY --from=web /web/dist web/dist
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p moli-os && cp target/release/moli-os /moli-os

FROM scratch
COPY --from=build /moli-os /moli-os
ENV MOLI_CONFIG=/data/moli.toml
WORKDIR /data
USER 1000:1000
EXPOSE 8790
ENTRYPOINT ["/moli-os"]
CMD ["serve"]
