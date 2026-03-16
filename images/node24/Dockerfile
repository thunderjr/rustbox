FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin rustbox-agent

FROM node:24-bookworm-slim
COPY --from=builder /src/target/release/rustbox-agent /usr/bin/rustbox-agent
EXPOSE 5123
CMD ["/usr/bin/rustbox-agent"]
