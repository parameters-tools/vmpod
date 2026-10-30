# vmpod (v1.0.0)

[![Rust Version](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Docker](https://img.shields.io/badge/docker-bollard%20client-2496ED.svg)](https://github.com/fussybeaver/bollard)
[![Hypervisor](https://img.shields.io/badge/hypervisor-QEMU%20%2B%20KVM-red.svg)](https://www.qemu.org/)

**vmpod** is a fast, local-first, zero-dependency CLI written in Rust that spins up and orchestrates hardware-accelerated Linux virtual machines running inside Docker containers using **QEMU** and **KVM**.

---

## Architecture Overview

```
+----------------------------------------------------------------------------------+
| HOST MACHINE (Linux / Ubuntu)                                                    |
|                                                                                  |
|   +-----------------------+     Direct Unix Socket IPC                           |
|   |   vmpod CLI (Rust)    | ------------------------------+                      |
|   |   - Tokio runtime     |   /var/run/docker.sock        |                      |
|   |   - Bollard client    |                               |                      |
|   +-----------------------+                               v                      |
|                                            +-------------------------------+     |
|   Host Storage: ./vmpod-data/              | Docker Daemon (dockerd)       |     |
|     ├── disk.qcow2 (qcow2 overlay)         +-------------------------------+     |
|     └── id_ed25519 (SSH private key)                      |                      |
|               ^                                           |                      |
|               | Direct Bind Mount (/storage)              v                      |
|   +--------------------------------------------------------------------------+   |
|   | DOCKER RUNNER CONTAINER (vmpod/qemu-runner:latest)                       |   |
|   |                                                                          |   |
|   |   Devices:                                                               |   |
|   |     - /dev/kvm       ──> Host Kernel KVM subsystem (Hardware Accel)      |   |
|   |     - /dev/net/tun   ──> Virtual Network Tunneling interface             |   |
|   |   Capabilities: NET_ADMIN                                                |   |
|   |   Port Map:     Host 0.0.0.0:2222 ──> Container :22                      |   |
|   |                                                                          |   |
|   |   +------------------------------------------------------------------+   |   |
|   |   | QEMU Process (qemu-system-x86_64)                                |   |   |
|   |   |   - Flags: -enable-kvm -cpu host -m 2G -smp 2 -nographic         |   |   |
|   |   |   - SLIRP Net: hostfwd=tcp::22-:22                               |   |   |
|   |   |                                                                  |   |   |
|   |   |   +----------------------------------------------------------+   |   |   |
|   |   |   | GUEST VIRTUAL MACHINE (Alpine Linux 3.20)                |   |   |   |
|   |   |   |   - Guest Kernel: vmlinuz-virt (runs on bare-metal CPU)  |   |   |   |
|   |   |   |   - OpenSSH Server: Port 22                              |   |   |   |
|   |   |   |   - Disk: /dev/vda (mounted to host's disk.qcow2)        |   |   |   |
|   |   |   +----------------------------------------------------------+   |   |   |
|   |   +------------------------------------------------------------------+   |   |
|   +--------------------------------------------------------------------------+   |
+----------------------------------------------------------------------------------+
```

### Key Engineering Features
- **Direct Bollard Socket Integration:** Directly talks to `/var/run/docker.sock` over Unix domain sockets using `bollard`. Zero shelling out to the `docker` binary.
- **Hardware-Accelerated Virtualization:** Full KVM pass-through (`/dev/kvm`) with near-native CPU and memory execution speeds.
- **Fail-Fast Pre-Flight Engine:** Validates host `/dev/kvm` presence, device node permissions, and Docker daemon connectivity before initiating container allocation.
- **Direct Host Volume Bypassing Overlay2:** Binds host storage directories directly to avoid Docker storage driver overhead and prevent filesystem nested copy-on-write degradation.
- **User-Space Networking (SLIRP):** QEMU user networking maps host/container TCP port 22 directly into the guest VM without needing privileged host bridge interfaces.
- **Automatic Root Credentials & SSH Key Provisioning:** Provisions guest root credentials (password `root` and auto-generated Ed25519 SSH keys), exporting the host SSH private key directly into `<STORAGE>/id_ed25519` with matched host user UID permissions.
- **Graceful Shutdown:** `vmpod down` sends a graceful stop request with a 5-second timeout allowing QEMU to flush all virtual disk caches before container removal.

---

## Prerequisites

1. **Linux with KVM Enabled:**
   Verify that KVM virtualization is supported and loaded:
   ```bash
   # Check if KVM device is present
   ls -l /dev/kvm

   # If missing, ensure hardware virtualization (VT-x/AMD-V) is enabled in BIOS and load modules:
   sudo modprobe kvm
   sudo modprobe kvm_intel   # For Intel CPUs
   # or
   sudo modprobe kvm_amd     # For AMD CPUs
   ```

2. **User Permissions:**
   Ensure your host user has access to both `kvm` and `docker` groups:
   ```bash
   sudo usermod -aG kvm $USER
   sudo usermod -aG docker $USER
   ```
   *(Log out and log back in for group changes to take effect)*.

3. **Rust Toolchain:**
   - Rust 1.75+ (2021 Edition) and Cargo.

---

## Installation & Build

### 1. Build the Docker Runner Image
From the repository root:
```bash
docker build -t vmpod/qemu-runner:latest .
```

### 2. Build and Install the Rust CLI Globally
```bash
cargo install --path .
```
This places `vmpod` directly into `~/.cargo/bin/vmpod` (which is typically in your `$PATH`).

Alternatively, build the binary:
```bash
cargo build --release
sudo cp target/release/vmpod /usr/local/bin/
```

---

## Step-by-Step Quickstart

### Step 1: Spin Up the VM
```bash
vmpod up
```
*Output:*
```text
============================================================
 Launching VM Instance: vmpod-box
============================================================

[✓] Virtual Machine successfully started!
------------------------------------------------------------
  Name:          vmpod-box
  Container ID:  47c9b962e820 (47c9b962e82098d3992416bfa6857ba2d335429f1ad5439760ed856aa07406e5)
  Status:        running
  Storage Path:  /home/kevin/vmpod-data
  SSH Command:   ssh root@localhost -p 2222
------------------------------------------------------------
Connect via SSH:
  ssh root@localhost -p 2222
  (Default credentials: user 'root', password 'root')
  (Or using private key: ssh -i /home/kevin/vmpod-data/id_ed25519 root@localhost -p 2222)
```

### Step 2: Connect via SSH

#### Option A: Passwordless Key Login (Recommended)
```bash
ssh -i ./vmpod-data/id_ed25519 root@localhost -p 2222
```

#### Option B: Password Login
```bash
ssh root@localhost -p 2222
```
*Password:* `root`

### Step 3: Check Status
```bash
vmpod status
```

### Step 4: Stop and Remove VM
```bash
vmpod down
```
*(Your disk image, installed packages, and files remain safely in `./vmpod-data` for the next `vmpod up`).*

---

## Subcommand Reference

### 1. `vmpod up [OPTIONS]`
Spin up a new hardware-accelerated virtual machine container.

| Flag | Default | Description |
| :--- | :--- | :--- |
| `--name <NAME>` | `vmpod-box` | Unique identifier for the VM container |
| `--port <PORT>` | `2222` | Host port mapped to the VM's SSH port 22 |
| `--ram <RAM>` | `2G` | Guest RAM allocation (e.g. `2G`, `4G`, `8G`) |
| `--smp <CPUS>` | `2` | Guest vCPU core count |
| `--storage <PATH>`| `./vmpod-data` | Host directory where virtual disk images live |
| `--image <IMAGE>` | `vmpod/qemu-runner:latest` | Docker image running QEMU |

**Example with custom resources:**
```bash
vmpod up --name dev-vm --port 2244 --ram 8G --smp 4 --storage ~/my-vms/dev
```

### 2. `vmpod status [OPTIONS]`
Inspect the live status, uptime, container ID, and port bindings of an instance.

| Flag | Default | Description |
| :--- | :--- | :--- |
| `--name <NAME>` | `vmpod-box` | Name of the VM instance to inspect |

**Example:**
```bash
vmpod status --name dev-vm
```

### 3. `vmpod down [OPTIONS]`
Gracefully shut down QEMU (allowing disk caches to flush) and tear down the container.

| Flag | Default | Description |
| :--- | :--- | :--- |
| `--name <NAME>` | `vmpod-box` | Name of the VM instance to tear down |

**Example:**
```bash
vmpod down --name dev-vm
```

---

## Advanced Usage

### 1. Running Docker Inside the VM (Nested Docker)
Because `vmpod` runs a true hardware-virtualized Linux kernel, you can run nested Docker inside the VM!

> [!TIP]
> **Subnet Tip:** To prevent the guest Docker bridge from colliding with your host laptop's Docker bridge (`172.17.0.1`), configure `/etc/docker/daemon.json` inside the VM before starting Docker:

```bash
# Inside the VM:
apk add docker

mkdir -p /etc/docker
cat << 'EOF' > /etc/docker/daemon.json
{
  "bip": "172.28.0.1/16"
}
EOF

service docker start
docker run --rm hello-world
```

### 2. Bring Your Own Linux Cloud Image
You can use any standard cloud or raw virtual disk image (Ubuntu, Debian, Fedora, Arch, etc.):

```bash
# 1. Create a storage folder
mkdir -p ./my-ubuntu-vm

# 2. Download any cloud qcow2 image into storage directory
curl -L -o ./my-ubuntu-vm/disk.qcow2 \
  https://cloud-images.ubuntu.com/minimal/releases/noble/release/ubuntu-24.04-minimal-cloudimg-amd64.img

# 3. Launch via vmpod
vmpod up --name ubuntu-box --port 2223 --storage ./my-ubuntu-vm
```
`vmpod` automatically mounts `./my-ubuntu-vm/disk.qcow2` as the primary drive, provisions root SSH credentials via cloud-init, and boots it under KVM.

---

## File Structure

```
vmpod/
├── Cargo.toml            # Fully pinned dependencies (tokio, clap, bollard, anyhow)
├── Dockerfile            # Multi-layer Alpine/QEMU runner container
├── entrypoint.sh         # TUN/KVM configuration, dynamic disk init & QEMU runner
├── README.md             # Production guide and documentation
└── src/
    ├── main.rs           # CLI definition, routing, and formatted terminal output
    └── orchestrator.rs   # Encapsulated Bollard orchestration & pre-flight engine
```

---

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
