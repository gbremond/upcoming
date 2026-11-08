# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked \
 && cp target/release/upcoming /upcoming

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /upcoming /usr/local/bin/upcoming
WORKDIR /data
ENV TZ=Europe/Paris RUST_LOG=upcoming=info
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/upcoming"]