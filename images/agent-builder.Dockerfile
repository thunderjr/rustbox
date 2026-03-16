FROM rust:1-bookworm AS builder
WORKDIR /src
COPY . .
RUN cargo build --release --bin rustbox-agent
