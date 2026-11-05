FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY contracts/plans.json ./contracts/plans.json
RUN cargo build --locked --release -p ramwarden-api

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/* && useradd --system --uid 10001 warden
COPY --from=build /src/target/release/ramwarden-api /usr/local/bin/ramwarden-api
USER warden
ENV RAMWARDEN_API_BIND=0.0.0.0:8080
ENV PORT=8080
EXPOSE 8080
CMD ["ramwarden-api"]
