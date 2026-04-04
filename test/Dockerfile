FROM debian:bookworm-slim

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
    && rm -rf /var/lib/apt/lists/*

COPY . /opt/btrshot

ENTRYPOINT ["/opt/btrshot/test/entrypoint.sh"]
