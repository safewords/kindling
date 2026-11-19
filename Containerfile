# syntax=docker/dockerfile:1
#
# kindling, as the image CI publishes to ghcr.io/safewords/kindling.
#
# Nothing about any network is in it. Configure it at runtime:
#   - service settings: /etc/kindling/kindling.toml (KINDLING_CONFIG), or the
#     environment (PXE_SERVER_IP, PXE_API_TOKEN, …) — see kindling.example.toml;
#   - the database (policy, revisions, inventory): a volume at /srv/kindling/storage;
#   - boot loaders and images: a volume at /srv/kindling/tftproot.
# The boot policy lives in the database; change it with the web UI or
#   docker exec kindling pxe pxe:rule …
#
# Proxy DHCP and TFTP are broadcast and well-known UDP ports, so run it with
# host networking:
#   docker run -d --name kindling --network host \
#     -v ./kindling.toml:/etc/kindling/kindling.toml:ro \
#     -v kindling-storage:/srv/kindling/storage -v ./tftproot:/srv/kindling/tftproot:ro \
#     ghcr.io/safewords/kindling:latest

FROM node:22-bookworm-slim AS frontend
WORKDIR /src
COPY package.json package-lock.json vite.config.js ./
RUN npm ci --no-audit --no-fund
COPY resources resources
RUN npm run build

FROM rust:1.100.0-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY ipxe ipxe
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --bin pxe \
 && cp target/release/pxe /usr/local/bin/pxe

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /srv/kindling
COPY --from=build /usr/local/bin/pxe /usr/local/bin/pxe
# Read at runtime, relative to the working directory: the admin UI's
# templates and its built bundle.
COPY resources/views resources/views
COPY --from=frontend /src/public/build public/build
RUN mkdir -p storage tftproot /etc/kindling
ENV KINDLING_CONFIG=/etc/kindling/kindling.toml
VOLUME ["/srv/kindling/storage"]
EXPOSE 8080/tcp 67/udp 69/udp 4011/udp
ENTRYPOINT ["pxe"]
CMD ["pxe:serve"]
