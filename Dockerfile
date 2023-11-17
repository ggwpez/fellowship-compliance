FROM rust:1.72 as builder
# The exact rust version comes from the toolchain file.
WORKDIR /opt/fellows
COPY . .
RUN cargo install --path .

FROM debian:bullseye-slim
COPY --from=builder /usr/local/cargo/bin/fellows /usr/local/bin/fellows
COPY static static

EXPOSE 443
EXPOSE 80
CMD ["fellows"]
