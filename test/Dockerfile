FROM ubuntu:24.04

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
        btrfs-progs \
        gnupg \
        util-linux \
        coreutils \
        bash \
        tar \
        gzip \
        findutils \
        grep \
        sed \
        gawk \
        procps \
        iproute2 \
        kmod \
        awscli \
        ca-certificates \
        curl \
    && rm -rf /var/lib/apt/lists/*

# Install MinIO server (static binary, architecture-aware)
ARG TARGETARCH
RUN curl -fsSL "https://dl.min.io/server/minio/release/linux-${TARGETARCH}/minio" -o /usr/local/bin/minio \
    && chmod +x /usr/local/bin/minio

ENTRYPOINT ["/opt/btrshot/test/entrypoint.sh"]
