# syntax=docker/dockerfile:1
FROM rust:1-bookworm AS chef
RUN cargo install cargo-chef --version 0.1.78 --locked
WORKDIR /src

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS build
COPY --from=planner /src/recipe.json recipe.json
# Keep compiled dependencies in the layer exported to the CI cache.
RUN cargo chef cook --release --locked --recipe-path recipe.json
COPY . .
RUN cargo build --release --locked \
 && cp target/release/upcoming /upcoming

FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /upcoming /usr/local/bin/upcoming
WORKDIR /data
ENV TZ=Europe/Paris RUST_LOG=upcoming=info
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/upcoming"]