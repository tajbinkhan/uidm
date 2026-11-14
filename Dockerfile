# UDM on a server or NAS, without a window (docs/server.md).
#
#   docker build -t udm .
#   docker run -d --name udm -v udm-config:/config -v /srv/downloads:/downloads udm
#   docker exec udm udm-cli get https://example.com/file.zip --no-wait
#
# The daemon runs in server mode: it stays up, logs to standard output
# (docker logs), and listens on 127.0.0.1 inside the container only, so the
# image publishes no port. udm-cli and udm-mcp in the container reach
# it through `docker exec`.
#
# Build with --build-arg FFMPEG=1 to add ffmpeg, which joins the video and
# audio of HLS and DASH streams.

# The build: without the window, but the file pickers' GTK (rfd, in
# udm-infrastructure) is still compiled, so the GTK headers are needed here
# but not in the image.
FROM rust:1-slim-bookworm AS build
RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config libssl-dev libgtk-3-dev \
 && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p udm-app --no-default-features \
      --bin udm-daemon --bin udm-cli --bin udm-mcp \
 && mkdir -p /out \
 && cp target/release/udm-daemon target/release/udm-cli target/release/udm-mcp /out/

# The image: the three programs, OpenSSL for HTTPS and FTPS, the CA
# certificates, the time zones (TZ sets the one queue schedules use), and
# tini, which hands `docker stop`'s signal to the daemon so it saves its
# list before it exits.
FROM debian:bookworm-slim
ARG FFMPEG=0
ARG DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3 tzdata tini \
 && if [ "$FFMPEG" = 1 ]; then apt-get install -y --no-install-recommends ffmpeg; fi \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --uid 1000 --user-group --create-home --home-dir /home/udm udm \
 && mkdir -p /config /downloads \
 && chown udm:udm /config /downloads
COPY --from=build /out/ /usr/local/bin/
# The country table's data is CC BY 4.0, whose notice goes with it.
COPY THIRD-PARTY-NOTICES.txt /usr/share/doc/udm/

# Every program in the container finds the same folders, so `docker exec`
# reaches the running daemon with its token.
ENV UDM_CONFIG_DIR=/config \
    UDM_DATA_DIR=/config/data \
    UDM_DOWNLOADS_DIR=/downloads
VOLUME ["/config", "/downloads"]
USER udm
WORKDIR /downloads

LABEL org.opencontainers.image.title="UDM" \
      org.opencontainers.image.description="UDM's download daemon in server mode" \
      org.opencontainers.image.source="https://github.com/tajbinkhan/UDM" \
      org.opencontainers.image.licenses="MIT AND CC-BY-4.0"

ENTRYPOINT ["/usr/bin/tini", "--", "udm-daemon", "--server"]
