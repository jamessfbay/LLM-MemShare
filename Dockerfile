FROM rust:1.89-bookworm AS builder
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src \
    && touch src/lib.rs \
    && printf 'fn main() {}\n' > src/main.rs \
    && cargo build --release --locked \
    && rm -rf src
COPY migrations ./migrations
COPY contracts ./contracts
COPY src ./src
RUN touch src/lib.rs src/main.rs \
    && cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --create-home --uid 10001 memshare
COPY --from=builder /build/target/release/llm-memshare /usr/local/bin/memshare
USER 10001:10001
EXPOSE 8080
ENTRYPOINT ["memshare"]
CMD ["serve", "--bind", "0.0.0.0:8080"]
