#!/usr/bin/env bash
set -eo pipefail

echo "============================================================"
echo " Starting vmpod QEMU Runner Container"
echo "============================================================"

# 1. Setup /dev/net/tun if missing
if [ ! -c /dev/net/tun ]; then
    echo "[*] Creating /dev/net/tun device node..."
    mkdir -p /dev/net
    mknod /dev/net/tun c 10 200
    chmod 666 /dev/net/tun
fi

# 2. Check KVM hardware acceleration
if [ -c /dev/kvm ]; then
    echo "[*] KVM acceleration is available: /dev/kvm"
    ACCEL_ARGS="-enable-kvm -cpu host"
else
    echo "[!] /dev/kvm not found or not accessible. Falling back to TCG emulation."
    ACCEL_ARGS="-cpu max"
fi

# 3. Configure storage and disk image
STORAGE_DIR="${STORAGE_DIR:-/storage}"
mkdir -p "${STORAGE_DIR}"

# Determine guest disk image
DISK_PATH=""
if [ -n "${DISK_IMAGE}" ] && [ -f "${STORAGE_DIR}/${DISK_IMAGE}" ]; then
    DISK_PATH="${STORAGE_DIR}/${DISK_IMAGE}"
elif [ -f "${STORAGE_DIR}/disk.qcow2" ]; then
    DISK_PATH="${STORAGE_DIR}/disk.qcow2"
else
    # Look for any existing qcow2 or raw image in storage
    EXISTING_IMG=$(find "${STORAGE_DIR}" -maxdepth 1 \( -name "*.qcow2" -o -name "*.img" -o -name "*.raw" \) 2>/dev/null | head -n 1)
    if [ -n "${EXISTING_IMG}" ]; then
        DISK_PATH="${EXISTING_IMG}"
    else
        DISK_PATH="${STORAGE_DIR}/disk.qcow2"
        if [ -f "/opt/vmpod/base-alpine.qcow2" ]; then
            echo "[*] Initializing guest disk from base Alpine template..."
            qemu-img create -f qcow2 -F qcow2 -b "/opt/vmpod/base-alpine.qcow2" "${DISK_PATH}" 10G
        else
            echo "[*] Creating new 10G qcow2 virtual disk: ${DISK_PATH}"
            qemu-img create -f qcow2 "${DISK_PATH}" 10G
        fi
    fi
fi

echo "[*] Using virtual disk: ${DISK_PATH}"

# 4. Auto-provision default root SSH credentials
mkdir -p /root/.ssh
chmod 700 /root/.ssh
if [ ! -f /root/.ssh/id_ed25519 ]; then
    echo "[*] Generating default ed25519 SSH keypair..."
    ssh-keygen -t ed25519 -N "" -f /root/.ssh/id_ed25519 -C "vmpod-root-key"
fi
cat /root/.ssh/id_ed25519.pub >> /root/.ssh/authorized_keys
chmod 600 /root/.ssh/authorized_keys

# Copy private key to storage for convenient host access if not already present
if [ ! -f "${STORAGE_DIR}/id_ed25519" ]; then
    cp /root/.ssh/id_ed25519 "${STORAGE_DIR}/id_ed25519"
    cp /root/.ssh/id_ed25519.pub "${STORAGE_DIR}/id_ed25519.pub"
    chmod 600 "${STORAGE_DIR}/id_ed25519"
    echo "[*] SSH private key copied to host storage: ${STORAGE_DIR}/id_ed25519"
fi

# 5. Generate cloud-init / nocloud CIDATA disk to provision root user & SSH key in guest
CIDATA_ARGS=""
if command -v genisoimage >/dev/null 2>&1; then
    CIDATA_DIR=$(mktemp -d /tmp/cidata.XXXXXX)
    PUBKEY=$(cat /root/.ssh/id_ed25519.pub)

    cat <<EOF > "${CIDATA_DIR}/meta-data"
instance-id: ${HOSTNAME:-vmpod-box}
local-hostname: ${HOSTNAME:-vmpod-box}
EOF

    cat <<EOF > "${CIDATA_DIR}/user-data"
#cloud-config
ssh_pwauth: true
disable_root: false
chpasswd:
  list: |
    root:root
  expire: false
ssh_authorized_keys:
  - ${PUBKEY}
runcmd:
  - echo "PermitRootLogin yes" >> /etc/ssh/sshd_config
  - rc-service sshd restart || true
EOF

    genisoimage -output /tmp/cidata.iso -volid cidata -joliet -rock "${CIDATA_DIR}/user-data" "${CIDATA_DIR}/meta-data" >/dev/null 2>&1
    rm -rf "${CIDATA_DIR}"
    CIDATA_ARGS="-drive file=/tmp/cidata.iso,format=raw,if=virtio"
    echo "[*] Cloud-init / nocloud seed image provisioned with root credentials."
fi

# 6. Parse resource limits from environment
RAM_ALLOC="${RAM:-2G}"
SMP_ALLOC="${SMP:-2}"
SSH_PORT="${SSH_PORT:-22}"

echo "[*] VM Specs: RAM=${RAM_ALLOC}, SMP=${SMP_ALLOC}"
echo "[*] Networking: User-space SLIRP with hostfwd tcp::${SSH_PORT}-:22"

# 7. Setup signal handling for graceful QEMU shutdown
QEMU_PID=""
cleanup() {
    echo ""
    echo "[*] Received termination signal. Gracefully stopping QEMU..."
    if [ -n "$QEMU_PID" ]; then
        kill -TERM "$QEMU_PID" 2>/dev/null || true
        wait "$QEMU_PID" 2>/dev/null || true
    fi
    echo "[*] QEMU stopped cleanly."
    exit 0
}
trap cleanup SIGTERM SIGINT

# 8. Launch QEMU
echo "[*] Executing QEMU..."
qemu-system-x86_64 \
    ${ACCEL_ARGS} \
    -m "${RAM_ALLOC}" \
    -smp "${SMP_ALLOC}" \
    -nographic \
    -serial mon:stdio \
    -drive "file=${DISK_PATH},format=qcow2,if=virtio" \
    ${CIDATA_ARGS} \
    -netdev "user,id=net0,hostfwd=tcp::${SSH_PORT}-:22" \
    -device "virtio-net-pci,netdev=net0" &

QEMU_PID=$!
wait "$QEMU_PID"
