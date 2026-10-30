# vmpod (v1.0.0)

[![Rust Version](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Docker](https://img.shields.io/badge/docker-bollard%20client-2496ED.svg)](https://github.com/fussybeaver/bollard)
[![Hypervisor](https://img.shields.io/badge/hypervisor-QEMU%20%2B%20KVM-red.svg)](https://www.qemu.org/)

**vmpod** is a fast, local-first, zero-dependency CLI written in Rust that spins up and orchestrates hardware-accelerated Linux virtual machines running inside Docker containers using **QEMU** and **KVM**.

---

## Architecture Overview

```
+-------------------------------------------------------------------------+
| Host Machine                                                            |
|                                                                         |
|  +-------------------+       Unix Socket (/var/run/docker.sock)         |
|  |   vmpod CLI       | =============================================>   |
|  | (Rust 2021/Tokio) |                                                  |
|  +-------------------+                                                  |
|           |                                                             |
|           | SSH Client: ssh root@localhost -p 2222                      |
|           v                                                             |
|  +-------------------------------------------------------------------+  |
|  | Docker Container: vmpod-box (vmpod/qemu-runner:latest)            |  |
|  |                                                                   |  |
|  |   - Devices: /dev/kvm (rwm), /dev/net/tun (rwm)                   |  |
|  |   - Cap:     NET_ADMIN                                            |  |
|  |   - Volume:  Host <STORAGE> bound to /storage (bypasses overlay2) |  |
|  |   - Port:    Host <PORT> -> Container 22                          |  |
|  |                                                                   |  |
|  |   +-------------------------------------------------------------+ |  |
|  |   | QEMU Process (-enable-kvm -cpu host -nographic)             | |  |
|  |   |                                                             | |  |
|  |   |   SLIRP Networking: hostfwd=tcp::22-:22                     | |  |
|  |   |   Drives: /storage/disk.qcow2 + /tmp/cidata.iso (seed)      | |  |
|  |   |                                                             | |  |
|  |   |   +-----------------------------------------------------+   | |  |
|  |   |   | Linux Guest VM (Hardware-Accelerated via KVM)       |   | |  |
|  |   |   | OpenSSH Server listening on port 22                 |   | |  |
|  |   |   | Provisioned root user & auto-generated SSH key      |   | |  |
|  |   |   +-----------------------------------------------------+   | |  |
|  |   +-------------------------------------------------------------+ |  |
|  +-------------------------------------------------------------------+  |
+-------------------------------------------------------------------------+
```

### Key Engineering Features
- **Direct Bollard Socket Integration:** Directly talks to `/var/run/docker.sock` over Unix domain sockets using `bollard`. Zero shelling out to the `docker` binary.
- **Hardware-Accelerated Virtualization:** Full KVM pass-through (`/dev/kvm`) with near-native CPU and memory execution speeds.
- **Fail-Fast Pre-Flight Engine:** Validates host `/dev/kvm` presence, device node permissions, and Docker daemon connectivity before initiating container allocation.
- **Direct Host Volume Bypassing Overlay2:** Binds host storage directories directly to avoid Docker storage driver overhead and prevent filesystem nested copy-on-write degradation.
- **User-Space Networking (SLIRP):** QEMU user networking maps host/container TCP port 22 directly into the guest VM without needing privileged host bridge interfaces.
- **Automatic Cloud-Init Seed:** Automatically provisions guest root credentials (password `root` and auto-generated Ed25519 SSH keys) via a dynamic CIDATA volume, exporting the host SSH private key directly into `<STORAGE>/id_ed25519`.
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

### 2. Build the Rust CLI
```bash
cargo build --release
```
The optimized binary will be created at `target/release/vmpod`. You can copy it to `/usr/local/bin`:
```bash
sudo cp target/release/vmpod /usr/local/bin/
```

---

## Subcommand Reference

### 1. `vmpod up`
Spin up a new hardware-accelerated virtual machine container.

```bash
vmpod up [OPTIONS]
```

**Options:**
- `--name <NAME>`: Unique identifier for the VM container (default: `vmpod-box`).
- `--port <PORT>`: Host port mapped to the VM's SSH port 22 (default: `2222`).
- `--ram <RAM>`: Guest RAM allocation, passed as env var (default: `2G`).
- `--smp <CPUS>`: Guest vCPU core count, passed as env var (default: `2`).
- `--storage <PATH>`: Host directory where virtual disk images live (default: `./vmpod-data`).
- `--image <IMAGE>`: Docker image running QEMU (default: `vmpod/qemu-runner:latest`).

**Sample Output:**
```
============================================================
 Launching VM Instance: vmpod-box
============================================================

[✓] Virtual Machine successfully started!
------------------------------------------------------------
  Name:          vmpod-box
  Container ID:  06215f8b612a (06215f8b612a5c86f69f35aba6bd4195b0e179bce7d9055206e6ae0514f5a042)
  Status:        running
  Storage Path:  /home/kevin/vmpod/vmpod-data
  SSH Command:   ssh root@localhost -p 2222
------------------------------------------------------------
Connect via SSH:
  ssh root@localhost -p 2222
  (Default credentials: user 'root', password 'root')
  (Or using private key: ssh -i /home/kevin/vmpod/vmpod-data/id_ed25519 root@localhost -p 2222)
```

---

### 2. `vmpod status`
Inspect the live status, uptime, container ID, and port bindings of an instance.

```bash
vmpod status [OPTIONS]
```

**Options:**
- `--name <NAME>`: Name of the VM instance to inspect (default: `vmpod-box`).

**Sample Output:**
```
============================================================
 Instance Status: vmpod-box
============================================================
  Container ID:  06215f8b612a (06215f8b612a5c86f69f35aba6bd4195b0e179bce7d9055206e6ae0514f5a042)
  Status:        ● running (active)
  Uptime:        3m 42s
  Image:         vmpod/qemu-runner:latest
  Storage Bind:  /home/kevin/vmpod/vmpod-data:/storage:rw
  Port Mappings:
    - 0.0.0.0:2222 -> 22/tcp
  SSH Command:   ssh root@localhost -p 2222
------------------------------------------------------------
```

---

### 3. `vmpod down`
Gracefully shut down QEMU (allowing disk caches to flush) and tear down the container.

```bash
vmpod down [OPTIONS]
```

**Options:**
- `--name <NAME>`: Name of the VM instance to tear down (default: `vmpod-box`).

**Sample Output:**
```
============================================================
 Tearing Down VM Instance: vmpod-box
============================================================
[*] Gracefully stopping container 'vmpod-box' (5-second timeout for QEMU disk flush)...
[✓] Container 'vmpod-box' stopped.
[*] Removing container 'vmpod-box'...
[✓] Container 'vmpod-box' successfully removed.
```

---

## Usage Scenarios & Examples

### Default Alpine Quickstart
```bash
# Launch a default VM
vmpod up

# Connect using default password 'root' or auto-generated key:
ssh -i ./vmpod-data/id_ed25519 root@localhost -p 2222
# or
ssh root@localhost -p 2222
```

### High-Resource VM on Custom Port
```bash
vmpod up --name dev-vm --port 2200 --ram 8G --smp 4 --storage ~/vm-storage
```

### Bring Your Own Linux Cloud Image
You can use any standard cloud or raw virtual disk image (Ubuntu, Debian, Fedora, Arch, etc.):
```bash
# 1. Create storage folder
mkdir -p ./my-ubuntu-vm

# 2. Download any cloud qcow2 image into storage directory
curl -L -o ./my-ubuntu-vm/disk.qcow2 \
  https://cloud-images.ubuntu.com/minimal/releases/noble/release/ubuntu-24.04-minimal-cloudimg-amd64.img

# 3. Launch via vmpod
vmpod up --name ubuntu-box --port 2223 --storage ./my-ubuntu-vm
```
`vmpod` detects `./my-ubuntu-vm/disk.qcow2`, mounts it as the primary drive, injects root SSH credentials via cloud-init, and boots it under KVM.

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
