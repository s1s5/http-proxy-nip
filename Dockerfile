# ------------- build ----------------
FROM clux/muslrust:1.72.0 as builder

RUN mkdir -p /rust && mkdir -p /cargo
WORKDIR /rust

# ソースコードのコピー
COPY Cargo.toml Cargo.lock /rust/
COPY src /rust/src

# バイナリ名を変更すること
RUN --mount=type=cache,target=/rust/target \
    --mount=type=cache,target=/root/.cargo/registry \
    --mount=type=cache,target=/root/.cargo/git \
    cargo build --release && \
    cp /rust/target/x86_64-unknown-linux-musl/release/http-proxy /proxy

# ------------- server ----------------
FROM scratch AS proxy
COPY --from=builder /proxy /proxy
ENTRYPOINT [ "/proxy" ]
ENV RUST_LOG=info
