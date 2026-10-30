# syntax=docker/dockerfile:1
FROM alpine:3.20

LABEL maintainer="vmpod team" \
      description="Hardware-accelerated QEMU/KVM runner container for vmpod"

# Install required tools: QEMU, OpenRC, OpenSSH, curl, plus disk image and filesystem tools
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
    procps \
    e2fsprogs \
    e2fsprogs-extra

# Configure OpenRC and default SSH setup inside the container
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

# Download base Alpine cloud image and customize credentials directly in the ext4 rootfs
RUN curl -fsSL -o /opt/vmpod/download.qcow2 \
    https://dl-cdn.alpinelinux.org/alpine/v3.20/releases/cloud/nocloud_alpine-3.20.9-x86_64-bios-tiny-r0.qcow2 && \
    qemu-img convert -O raw /opt/vmpod/download.qcow2 /opt/vmpod/base.raw && \
    rm -f /opt/vmpod/download.qcow2 && \
    \
    # 1. Update /etc/shadow with 'root' password hash for root and alpine
    debugfs -R 'dump /etc/shadow /tmp/shadow.tmp' /opt/vmpod/base.raw && \
    sed -i 's|^root:.*|root:$6$vmpodroot$O/GItAt8vBrKo.sbIaza3WWZzdoVfdTTpob6WjyFh.KRXbwHnPJiY6nDpNK.FQ3N4kl2SO3lyKxjF3J.oXViT1:20481:0:99999:7:::|' /tmp/shadow.tmp && \
    sed -i 's|^alpine:.*|alpine:$6$vmpodroot$O/GItAt8vBrKo.sbIaza3WWZzdoVfdTTpob6WjyFh.KRXbwHnPJiY6nDpNK.FQ3N4kl2SO3lyKxjF3J.oXViT1:20481:0:99999:7:::|' /tmp/shadow.tmp && \
    debugfs -w -R 'rm /etc/shadow' /opt/vmpod/base.raw && \
    debugfs -w -R 'write /tmp/shadow.tmp /etc/shadow' /opt/vmpod/base.raw && \
    debugfs -w -R 'sif /etc/shadow mode 0100600' /opt/vmpod/base.raw && \
    \
    # 2. Update /etc/ssh/sshd_config to permit root login and password auth
    debugfs -R 'dump /etc/ssh/sshd_config /tmp/sshd.tmp' /opt/vmpod/base.raw && \
    printf "\nPermitRootLogin yes\nPasswordAuthentication yes\nPermitEmptyPasswords yes\n" >> /tmp/sshd.tmp && \
    debugfs -w -R 'rm /etc/ssh/sshd_config' /opt/vmpod/base.raw && \
    debugfs -w -R 'write /tmp/sshd.tmp /etc/ssh/sshd_config' /opt/vmpod/base.raw && \
    debugfs -w -R 'sif /etc/ssh/sshd_config mode 0100644' /opt/vmpod/base.raw && \
    \
    # 3. Create /root/.ssh and install default authorized_keys
    debugfs -w -R 'mkdir /root/.ssh' /opt/vmpod/base.raw 2>/dev/null || true && \
    debugfs -w -R 'sif /root/.ssh mode 040700' /opt/vmpod/base.raw && \
    debugfs -w -R 'write /root/.ssh/id_ed25519.pub /root/.ssh/authorized_keys' /opt/vmpod/base.raw && \
    debugfs -w -R 'sif /root/.ssh/authorized_keys mode 0100600' /opt/vmpod/base.raw && \
    \
    # 4. Clean up temp files and convert back to optimized qcow2 template
    rm -f /tmp/shadow.tmp /tmp/sshd.tmp && \
    qemu-img convert -O qcow2 -c /opt/vmpod/base.raw /opt/vmpod/base-alpine.qcow2 && \
    rm -f /opt/vmpod/base.raw

# Copy and configure entrypoint script
COPY entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh

# Expose guest SSH forwarding port
EXPOSE 22

VOLUME ["/storage"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
