# Drengr MCP server.
#
# The server completes the MCP handshake and serves its tool list with no device
# attached, connecting to one lazily on first use, so this image introspects
# cleanly.
#
# It also drives real hardware. adb is a TCP protocol, so the container never
# needs the USB bus: point it at an adb server that already has the device.
#
#   ADB_SERVER_SOCKET=tcp:<host>:5037   use a device attached to that host
#   adb connect <ip>:5555               use a device or emulator over TCP
#
# On Docker Desktop and Colima the host is reachable as host.docker.internal, so
# a phone plugged into the developer's laptop is one env var away:
#
#   host$      adb -a -P 5037 nodaemon server        # serve it on the bridge
#   container$ docker run -i --rm \
#                -e ADB_SERVER_SOCKET=tcp:host.docker.internal:5037 drengr
#
# `adb -a` binds every interface, so do that on a trusted network only, or bind
# one address with `adb -L tcp:<bridge-ip>:5037 nodaemon server`.

FROM rust:1-slim-bookworm AS build
WORKDIR /src

RUN apt-get update \
 && apt-get install -y --no-install-recommends pkg-config \
 && rm -rf /var/lib/apt/lists/*

COPY Cargo.toml Cargo.lock build.rs ./
COPY src ./src
# build.rs hashes drengr-runner/ and bootstrap.rs embeds it with include_dir!;
# mcp/mod.rs embeds the icon with include_str!. Both are compile-time inputs.
COPY drengr-runner ./drengr-runner
COPY assets ./assets

RUN cargo build --release --locked --bin drengr \
 && strip target/release/drengr

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates adb \
 && rm -rf /var/lib/apt/lists/*

RUN useradd --create-home --uid 10001 drengr
USER drengr
WORKDIR /home/drengr
ENV DRENGR_LOG_LEVEL=info

COPY --from=build /src/target/release/drengr /usr/local/bin/drengr

# stdio transport: the MCP client owns stdin and stdout.
ENTRYPOINT ["drengr"]
CMD ["mcp"]
