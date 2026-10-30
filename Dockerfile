# syntax=docker/dockerfile:1
FROM alpine:3.20

LABEL maintainer="vmpod team" \
      description="Hardware-accelerated QEMU/KVM runner container for vmpod"

# Install required tools: QEMU, OpenRC, OpenSSH, curl, plus image tools
RUN apk add --no-cache \
    qemu-system-x86_64 \
    qemu-img \
    openrc \
    openssh \
    openssh-server \
    curl \
    bash \
    cdrkit \
    ca-certificates \
    iproute2 \
    shadow \
    procps

# Configure OpenRC and default SSH setup
RUN mkdir -p /run/openrc && touch /run/openrc/softlevel && \
    mkdir -p /root/.ssh && chmod 700 /root/.ssh && \
    ssh-keygen -A && \
    ssh-keygen -t ed25519 -N "" -f /root/.ssh/id_ed25519 -C "vmpod-default-key" && \
    cat /root/.ssh/id_ed25519.pub >> /root/.ssh/authorized_keys && \
    chmod 600 /root/.ssh/authorized_keys && \
    echo "root:root" | chpasswd && \
    sed -i 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config 2>/dev/null || true

# Prepare storage mount point and base image cache directory
RUN mkdir -p /storage /opt/vmpod

# Download minimal Alpine cloud image to serve as an instant base template
# If the build host is offline or throttled, this step safely continues without failing
RUN curl -fsSL -o /opt/vmpod/base-alpine.qcow2 \
    https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.9-x86_64-bios-tiny-r0.qcow2 \
    || echo "[!] Base cloud image download skipped; entrypoint will initialize dynamic disk."

# Copy and configure entrypoint script
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Expose guest SSH forwarding port
EXPOSE 22

VOLUME ["/storage"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
