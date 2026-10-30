# vmpod (v1.2.0)

[![Rust Version](https://img.shields.io/badge/rust-2021%20edition-orange.svg)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-blue.svg)](LICENSE)
[![Docker](https://img.shields.io/badge/docker-bollard%20client-2496ED.svg)](https://github.com/fussybeaver/bollard)
[![Hypervisor](https://img.shields.io/badge/hypervisor-QEMU%20%2B%20KVM-red.svg)](https://www.qemu.org/)
[![TUI](https://img.shields.io/badge/tui-ratatui%20%2B%20crossterm-brightgreen.svg)](https://github.com/ratatui/ratatui)

**vmpod** is a fast, local-first, zero-dependency CLI written in Rust that spins up and orchestrates hardware-accelerated Linux virtual machines running inside Docker containers using **QEMU** and **KVM**.

Version **1.2.0** introduces a **Full Interactive Terminal User Interface (TUI) Dashboard** alongside multi-instance orchestration, providing live VM management, one-click interactive SSH sessions, real-time guest kernel log streaming, and modal creation/deletion workflows.

---

## Interactive TUI Dashboard (`vmpod dashboard` or `vmpod`)

Running `vmpod` with no arguments or typing `vmpod dashboard` (aliases: `vmpod tui`, `vmpod ui`) launches an interactive full-terminal management dashboard powered by **Ratatui**:

```text
╭ ⚡  VMPOD - Hardware-Accelerated Linux VM Orchestrator ───────────────────────╮
│    [↑/↓] Move [Enter] SSH [Space] Power [N] New [D] Del [R] Sync [Q] Quit    │
╰──────────────────────────────────────────────────────────────────────────────╯
╭ Virtual Machines (2) ────────╮╭ VM Instance Details ─────────────────────────╮
│▶  ● cluster-1      [:2222]   ││ cluster-1  ● RUNNING    Uptime: 14m 20s      │
│   ○ cluster-2      [off]     ││                                              │
│                              ││  vCPUs:         2 cores    Memory:  2G       │
│                              ││  SSH Port:      localhost:2222               │
│                              ││  Container ID:  c11122233344                 │
│                              ││  Storage Path:  /home/kevin/vmpod-data/cluste│
│                              ││  Virtual Disk:  196 KB (differential qcow2)  │
│                              ││  SSH Command:   ssh root@localhost -p 2222   │
╰──────────────────────────────╯╰──────────────────────────────────────────────╯
╭ 📜  Guest Kernel & Runner Console Logs ───────────────────────────────────────╮
│[ 0.000000] Linux version 6.6.121-0-virt (Alpine Linux)                       │
│[ 1.204512] eth0: leased 10.0.2.15 for 86400 seconds [ ok ]                   │
│[ 2.112004] Tiny Cloud: bootstrap complete. sshd listening on port 22         │
╰──────────────────────────────────────────────────────────────────────────────╯
 Ready.  │  1 running, 1 stopped  │  vmpod v1.2.0                               
```

### Dashboard Keyboard Controls:
- **`↑ / ↓` or `k / j`**: Navigate through all active and stopped virtual machines.
- **`Enter`**: Instantly launches an interactive SSH session attached directly to your terminal. Typing `exit` returns seamlessly to the dashboard.
- **`Space` or `s`**: Toggle VM power (starts stopped instances, or gracefully flushes disk caches and powers down running instances).
- **`N`**: Opens an interactive modal form to spin up a new VM (`Name`, `RAM`, `vCPUs`).
- **`D` or `Delete`**: Prompts for confirmation to delete a VM and purge its virtual disk.
- **`R`**: Forces an immediate refresh of Docker states, persistent disks, and logs.
- **`PgUp / PgDn`**: Scroll through guest kernel and runner logs.
- **`Q` or `Esc`**: Cleanly exit the dashboard.

---

## Multi-Instance Architecture Overview

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
|     ├── <instance-1>/                      +-------------------------------+     |
|     │     ├── disk.qcow2 (isolated overlay)               |                      |
|     │     └── id_ed25519 (SSH private key)                |                      |
|     └── <instance-2>/                                     |                      |
|           ├── disk.qcow2 (isolated overlay)               v                      |
|           └── id_ed25519 (SSH private key)                                       |
|                                                                                  |
|   +──────────────────────────────────────────────────────────────────────────+   |
|   │ VM Container 1: worker-1 (Port 2222 -> Container 22)                     │   |
|   │   - Labels: io.vmpod.managed=true, io.vmpod.name=worker-1                │   |
|   │   - Mount:  ./vmpod-data/worker-1 -> /storage (Direct NVMe/SSD)          │   |
|   │   - QEMU:   Hardware-accelerated Linux Guest VM (KVM)                    │   |
|   +──────────────────────────────────────────────────────────────────────────+   |
|   │ VM Container 2: worker-2 (Port 2223 -> Container 22)                     │   |
|   │   - Labels: io.vmpod.managed=true, io.vmpod.name=worker-2                │   |
|   │   - Mount:  ./vmpod-data/worker-2 -> /storage (Direct NVMe/SSD)          │   |
|   │   - QEMU:   Hardware-accelerated Linux Guest VM (KVM)                    │   |
|   +──────────────────────────────────────────────────────────────────────────+   |
+----------------------------------------------------------------------------------+
```

### Key Engineering Features
- **Multi-Instance Orchestration:** Concurrently run arbitrary numbers of isolated Linux VMs side-by-side with automatic port scanning and disk namespace isolation.
- **Docker Label-Based Global Discovery:** Containers are tagged with `io.vmpod.managed=true`, `io.vmpod.name`, and resource metadata, enabling seamless discovery via `vmpod list`.
- **Automatic Port Allocation:** Scans and binds free host SSH ports starting at `2222` when default or in-use ports are encountered.
- **Direct Bollard Socket Integration:** Directly talks to `/var/run/docker.sock` over Unix domain sockets using `bollard`. Zero shelling out to the `docker` binary.
- **Hardware-Accelerated Virtualization:** Direct KVM pass-through (`/dev/kvm`) with near-native CPU and memory execution speeds.
- **Fail-Fast Pre-Flight Engine:** Validates host `/dev/kvm` presence, device node permissions, and Docker daemon connectivity before initiating container allocation.
- **Direct Host Volume Bypassing Overlay2:** Binds host storage directories directly to avoid Docker storage driver overhead and prevent filesystem nested copy-on-write degradation.
- **User-Space Networking (SLIRP):** QEMU user networking maps host/container TCP ports directly into each guest VM without requiring privileged host bridge interfaces.
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

### 2. Install the Rust CLI Globally
```bash
cargo install --path .
```
This installs `vmpod` directly into `~/.cargo/bin/vmpod` (in your `$PATH`).

---

## Multi-Instance Quickstart Guide

### 1. Launch a Single VM
```bash
vmpod up
```
*Spins up a VM named `vmpod-box` mapped to port `2222` with storage in `./vmpod-data`.*

### 2. Launch Named VMs (Auto-Port & Storage Isolation)
```bash
# Launch a named VM - auto-allocates next free port (2223) and storage in ./vmpod-data/db-1
vmpod up db-1

# Launch another named VM - auto-allocates port 2224 and storage in ./vmpod-data/web-1
vmpod up web-1
```

### 3. Launch a Cluster with Batch Mode (`--count`)
```bash
# Spins up worker-1, worker-2, and worker-3 simultaneously!
vmpod up worker --count 3
```

### 4. List All Active Instances (`vmpod list` / `vmpod ls`)
```bash
vmpod list
```
**Example output:**
```text
=========================================================================================================
 MANAGED VMPOD INSTANCES
=========================================================================================================
 NAME             STATUS       PORT           CPUS     RAM        UPTIME         SSH COMMAND             
---------------------------------------------------------------------------------------------------------
 db-1             ● running    2223           2        2G         3m 12s         ssh root@localhost -p 2223
 vmpod-box        ● running    2222           2        2G         12m 45s        ssh root@localhost -p 2222
 web-1            ● running    2224           2        2G         2m 04s         ssh root@localhost -p 2224
 worker-1         ● running    2225           2        2G         48s            ssh root@localhost -p 2225
 worker-2         ● running    2226           2        2G         48s            ssh root@localhost -p 2226
 worker-3         ● running    2227           2        2G         48s            ssh root@localhost -p 2227
---------------------------------------------------------------------------------------------------------
Use 'vmpod status <NAME>' for details, or 'vmpod down <NAME>' to stop.
```

### 5. Connect via SSH
Connect to any instance using its assigned port and auto-generated private key:

```bash
# Connect to worker-1
ssh -i ./vmpod-data/worker-1/id_ed25519 root@localhost -p 2225

# Or via password 'root'
ssh root@localhost -p 2225
```

### 6. Inspect an Instance
```bash
# Detailed inspection
vmpod status worker-1

# Or run without arguments to view the global instance table
vmpod status
```

### 7. Stop and Tear Down Instances
```bash
# Stop a single instance
vmpod down worker-1

# Stop multiple specific instances
vmpod down worker-2 worker-3

# Tear down ALL instances on the host
vmpod down --all
```

---

## Subcommand Reference

### `vmpod up [NAME] [OPTIONS]`
Spin up hardware-accelerated virtual machine container(s).

| Argument / Flag | Default | Description |
| :--- | :--- | :--- |
| `[NAME]` | `vmpod-box` | Name or unique identifier for the VM container |
| `-c, --count <COUNT>` | `1` | Number of instances to spin up (creates `<NAME>-1`, `<NAME>-2`, etc.) |
| `--port <PORT>` | `auto` | Host port mapped to guest SSH (auto-detected if omitted or in use) |
| `--ram <RAM>` | `2G` | Guest RAM allocation (e.g. `2G`, `4G`, `8G`) |
| `--smp <CPUS>` | `2` | Guest vCPU core count |
| `--storage <PATH>` | `./vmpod-data/<NAME>` | Host directory where virtual disk images live |
| `--image <IMAGE>` | `vmpod/qemu-runner:latest` | Docker image running QEMU |

### `vmpod list` *(aliases: `ls`, `ps`)*
List all managed virtual machine instances on the host with status, port, uptime, and resource allocations.

### `vmpod status [NAME]`
Inspect the detailed status, uptime, storage mounts, and resource allocations of an instance. If `[NAME]` is omitted, displays the global instance table.

### `vmpod down [NAMES...] [OPTIONS]`
Gracefully shut down QEMU (allowing disk caches to flush) and remove containers.

| Argument / Flag | Default | Description |
| :--- | :--- | :--- |
| `[NAMES...]` | `vmpod-box` | One or more instance names to tear down |
| `-a, --all` | `false` | Stop and tear down all managed `vmpod` instances on the host |

---

## Advanced Usage

### Running Docker Inside the VM (Nested Docker)
Because `vmpod` runs a true hardware-virtualized Linux kernel, you can run nested Docker inside any VM!

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

### Bring Your Own Linux Cloud Image
You can use any standard cloud or raw virtual disk image (Ubuntu, Debian, Fedora, Arch, etc.):

```bash
# 1. Create a storage folder
mkdir -p ./my-ubuntu-vm

# 2. Download any cloud qcow2 image into storage directory
curl -L -o ./my-ubuntu-vm/disk.qcow2 \
  https://cloud-images.ubuntu.com/minimal/releases/noble/release/ubuntu-24.04-minimal-cloudimg-amd64.img

# 3. Launch via vmpod
vmpod up ubuntu-box --storage ./my-ubuntu-vm
```
`vmpod` automatically mounts `./my-ubuntu-vm/disk.qcow2` as the primary drive, provisions root SSH credentials via cloud-init, and boots it under KVM.

---

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.
