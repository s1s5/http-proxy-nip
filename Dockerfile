# docker buildx build --platform linux/amd64,linux/arm64 . -t s1s5/http-proxy-nip --push
# ------------- build ----------------
FROM --platform=$BUILDPLATFORM s1s5/musl:${TARGETARCH} as builder

RUN mkdir -p /rust && mkdir -p /cargo
WORKDIR /rust

# ソースコードのコピー
COPY Cargo.toml Cargo.lock /rust/
COPY src /rust/src

# multiplatform buildだと動かない
# RUN --mount=type=cache,target=/rust/target \
#     --mount=type=cache,target=/root/.cargo/registry \
#     --mount=type=cache,target=/root/.cargo/git \

RUN cargo build --release
RUN cp /rust/target/*-unknown-linux-musl/release/http-proxy /proxy

# ------------- server ----------------
FROM scratch AS proxy
COPY --from=builder /proxy /proxy
ENTRYPOINT [ "/proxy" ]
ENV RUST_LOG=info

