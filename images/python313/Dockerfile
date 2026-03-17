FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin rustbox-agent

FROM python:3.13-slim-bookworm
COPY --from=builder /src/target/release/rustbox-agent /usr/bin/rustbox-agent
COPY scripts/vm-init.sh /sbin/init
RUN chmod +x /sbin/init
EXPOSE 5123
CMD ["/usr/bin/rustbox-agent"]
