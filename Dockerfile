# Build stage
FROM rust:1.96-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p garnet-server

# Runtime stage. No Java in the image on purpose: Garnet downloads Mojang's
# own Java runtime into the data volume the first time it needs it.
FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --create-home --uid 1000 garnet
COPY --from=build /src/target/release/garnet /usr/local/bin/garnet
USER garnet
WORKDIR /data
VOLUME ["/data"]
# game (tcp), voice (udp), admin panel
EXPOSE 25565/tcp 25565/udp 8080/tcp
ENTRYPOINT ["garnet"]
