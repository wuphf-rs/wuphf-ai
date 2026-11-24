# syntax=docker/dockerfile:1
# Every wuphf binary of one release, in one image; each compose service runs
# its own (compose.yaml). The binaries are the release's, installed only once
# SHA256SUMS is signed by release/minisign.pub for that very release and every
# download is in it: the checks `wuphf-server update` makes.
#
#   docker build -f deploy/docker/Dockerfile --build-arg VERSION=0.2.0 .
#
# The build context is the repository; Dockerfile.dockerignore passes only
# what is copied.

ARG DEBIAN=debian:trixie-slim

FROM ${DEBIAN} AS fetch
ARG VERSION
ARG TARGETARCH
ARG REPOSITORY=wuphf-rs/wuphf-ai
RUN apt-get update -q \
    && apt-get install -y -q --no-install-recommends ca-certificates curl minisign \
    && rm -rf /var/lib/apt/lists/*
COPY release/minisign.pub /minisign.pub
WORKDIR /dist
RUN set -eu; \
    : "${VERSION:?--build-arg VERSION=X.Y.Z is required}"; \
    case "$TARGETARCH" in \
        amd64) arch=x86_64 ;; \
        arm64) arch=aarch64 ;; \
        *) echo "no release for $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    release="https://github.com/$REPOSITORY/releases/download/v$VERSION"; \
    binaries="wuphf-server wuphf-tui wuphf-telegram wuphf-whatsapp wuphf-whatsapp-rust wuphf-instagram wuphf-mediaconv wuphf-misc"; \
    curl -fsSL --retry 3 -O "$release/SHA256SUMS" -O "$release/SHA256SUMS.minisig"; \
    comment="$(minisign -V -q -Q -p /minisign.pub -m SHA256SUMS)"; \
    if [ "$comment" != "wuphf v$VERSION" ]; then \
        echo "SHA256SUMS is signed for '$comment', not wuphf v$VERSION" >&2; exit 1; \
    fi; \
    for binary in $binaries; do \
        curl -fsSL --retry 3 -o "$binary-linux-$arch" "$release/$binary-linux-$arch"; \
        grep -q " $binary-linux-$arch\$" SHA256SUMS || { echo "$binary-linux-$arch is not in SHA256SUMS" >&2; exit 1; }; \
    done; \
    sha256sum --check --ignore-missing --strict SHA256SUMS; \
    mkdir bin; \
    for binary in $binaries; do install -m 0755 "$binary-linux-$arch" "bin/$binary"; done

FROM ${DEBIAN}
ARG VERSION
# ffmpeg and ImageMagick (7: it decodes animated WebP) for wuphf-mediaconv;
# webpmux (webp) for WhatsApp sticker metadata; tini (entrypoint.sh).
RUN apt-get update -q \
    && apt-get install -y -q --no-install-recommends \
        ca-certificates ffmpeg imagemagick tini webp \
    && rm -rf /var/lib/apt/lists/*
COPY --from=fetch /dist/bin/ /usr/local/bin/
COPY deploy/docker/entrypoint.sh /usr/local/bin/wuphf-entrypoint
# Every part's state directory, so a fresh named volume starts with it.
RUN for part in core telegram whatsapp whatsapp-rust instagram mediaconv misc chats; do \
        mkdir -p "/var/lib/wuphf/$part"; \
    done \
    && mkdir -p /etc/wuphf
# The core's release check is off: an image is updated by pulling the next
# one, and `wuphf-server update` cannot replace its binaries.
ENV WUPHF_CHAT_DIRECTORY=/var/lib/wuphf/chats \
    WUPHF_CORE_UPDATES__CHECK_INTERVAL_SECONDS=0 \
    HOME=/tmp
WORKDIR /var/lib/wuphf/core
LABEL org.opencontainers.image.title="wuphf" \
      org.opencontainers.image.description="wuphf, a multi-platform messaging bridge: every binary of one release" \
      org.opencontainers.image.source="https://github.com/wuphf-rs/wuphf-ai" \
      org.opencontainers.image.licenses="MIT" \
      org.opencontainers.image.version="${VERSION}"
ENTRYPOINT ["wuphf-entrypoint"]
CMD ["wuphf-server", "serve", "--config", "/etc/wuphf/core.toml"]
