# Static ss-server. The build is native: amd64 produces the x86_64 musl
# binary, arm64 produces the aarch64 one. Cross builds live in the release
# workflow.

FROM rust:1.99.1-bookworm AS build

RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools binutils ca-certificates \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .

RUN set -eux; \
    case "$(uname -m)" in \
      x86_64) target=x86_64-unknown-linux-musl ;; \
      aarch64) target=aarch64-unknown-linux-musl ;; \
      *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;; \
    esac; \
    rustup target add "$target"; \
    export CC_x86_64_unknown_linux_musl=musl-gcc; \
    export CC_aarch64_unknown_linux_musl=musl-gcc; \
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc; \
    export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=musl-gcc; \
    cargo build -p ss-server --locked --release --target "$target"; \
    mkdir -p /out /data; \
    cp "target/${target}/release/ss-server" /out/ss-server; \
    strip --strip-unneeded /out/ss-server; \
    if readelf -l /out/ss-server | grep -q 'INTERP'; then \
      echo "ss-server is dynamically linked" >&2; \
      exit 1; \
    fi; \
    chown 65532:65532 /data

FROM gcr.io/distroless/static-debian12:nonroot

COPY --from=build /out/ss-server /ss-server
COPY --from=build --chown=65532:65532 /data /data

VOLUME ["/data"]
WORKDIR /data
EXPOSE 443/udp 443/tcp 9101/tcp

ENTRYPOINT ["/ss-server"]
CMD ["run", "--data-dir", "/data"]
