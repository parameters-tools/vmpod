use anyhow::{bail, Context, Result};
use bollard::models::{
    ContainerCreateBody, DeviceMapping, HostConfig, PortBinding, PortMap,
};
use bollard::query_parameters::{
    CreateContainerOptions, InspectContainerOptions, ListContainersOptionsBuilder,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::Docker;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

extern "C" {
    fn getuid() -> u32;
    fn getgid() -> u32;
}

pub const LABEL_MANAGED: &str = "io.vmpod.managed";
pub const LABEL_NAME: &str = "io.vmpod.name";
pub const LABEL_SSH_PORT: &str = "io.vmpod.ssh_port";
pub const LABEL_RAM: &str = "io.vmpod.ram";
pub const LABEL_SMP: &str = "io.vmpod.smp";

/// Parameters for spinning up a new VM container instance
pub struct UpParams {
    pub name: String,
    pub port: Option<u16>,
    pub ram: String,
    pub smp: u32,
    pub storage: Option<PathBuf>,
    pub image: String,
}

/// Result returned after a successful `up` operation
pub struct UpResult {
    pub container_id: String,
    pub short_id: String,
    pub name: String,
    pub port: u16,
    pub ssh_command: String,
    pub status: String,
    pub storage_path: PathBuf,
}

/// Result returned after inspecting a VM container
pub struct StatusResult {
    pub container_id: String,
    pub short_id: String,
    pub name: String,
    pub status: String,
    pub running: bool,
    pub uptime: String,
    pub port_mappings: Vec<String>,
    pub ssh_command: Option<String>,
    pub storage_bind: Option<String>,
    pub image: String,
    pub ram: Option<String>,
    pub smp: Option<String>,
}

/// Summary of an instance for listing
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct InstanceSummary {
    pub name: String,
    pub container_id: String,
    pub short_id: String,
    pub status: String,
    pub running: bool,
    pub port: Option<u16>,
    pub ssh_command: String,
    pub uptime: String,
    pub ram: String,
    pub smp: String,
    pub storage_path: String,
}

/// Orchestrator encapsulating Bollard Docker client interactions and hypervisor pre-flight checks
pub struct VmOrchestrator {
    docker: Docker,
}

impl VmOrchestrator {
    /// Initialize orchestrator connected to the local Docker Unix socket
    pub fn new() -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults().context(
            "Failed to initialize Docker client via direct Unix socket (/var/run/docker.sock)",
        )?;
        Ok(Self { docker })
    }

    /// Perform pre-flight verification:
    /// 1. Host `/dev/kvm` exists and has read/write permissions.
    /// 2. Docker daemon is reachable via `/var/run/docker.sock`.
    pub async fn preflight_check(&self) -> Result<()> {
        let kvm_path = Path::new("/dev/kvm");

        // Verify existence of /dev/kvm
        if !kvm_path.exists() {
            bail!(
                "Pre-flight check failed: '/dev/kvm' character device does not exist on host.\n\
                 Hardware-accelerated Linux virtualization requires KVM.\n\
                 Remediation steps:\n\
                   1. Verify CPU virtualization (Intel VT-x / AMD-V) is enabled in BIOS/UEFI.\n\
                   2. Load the KVM kernel modules:\n\
                        sudo modprobe kvm\n\
                        sudo modprobe kvm_intel   # (or kvm_amd for AMD CPUs)\n\
                   3. Verify /dev/kvm is created."
            );
        }

        // Verify read/write permissions to /dev/kvm
        if let Err(e) = OpenOptions::new().read(true).write(true).open(kvm_path) {
            bail!(
                "Pre-flight check failed: Cannot access '/dev/kvm' ({e}).\n\
                 Your user lacks read/write permissions for the KVM device node.\n\
                 Remediation step:\n\
                   sudo usermod -aG kvm $USER\n\
                 Then log out and back in to apply the group membership."
            );
        }

        // Verify Docker daemon connectivity
        self.docker.ping().await.context(
            "Pre-flight check failed: Unable to communicate with Docker daemon at /var/run/docker.sock.\n\
             Ensure the Docker service is active: 'sudo systemctl start docker'\n\
             And ensure your user belongs to the 'docker' group: 'sudo usermod -aG docker $USER'",
        )?;

        Ok(())
    }

    /// Find an available host port for SSH forwarding.
    /// If preferred is given, verifies it is free; otherwise scans starting from 2222.
    pub async fn find_available_port(&self, preferred: Option<u16>) -> Result<u16> {
        // Collect currently allocated host ports from Docker
        let containers = self
            .docker
            .list_containers(Some(
                ListContainersOptionsBuilder::default().all(true).build(),
            ))
            .await
            .unwrap_or_default();

        let mut occupied_docker_ports = std::collections::HashSet::new();
        for c in containers {
            if let Some(ports) = c.ports {
                for p in ports {
                    if let Some(pub_p) = p.public_port {
                        occupied_docker_ports.insert(pub_p);
                    }
                }
            }
        }

        let is_port_free = |port: u16| -> bool {
            if occupied_docker_ports.contains(&port) {
                return false;
            }
            // Verify TCP bind succeeds on 0.0.0.0 and 127.0.0.1
            match TcpListener::bind(("0.0.0.0", port)) {
                Ok(_) => true,
                Err(_) => false,
            }
        };

        if let Some(port) = preferred {
            if is_port_free(port) {
                return Ok(port);
            } else {
                bail!(
                    "Requested host port {} is already in use by another container or process.\n\
                     Please choose a different port via '--port <PORT>' or omit '--port' to auto-assign a free port.",
                    port
                );
            }
        }

        // Auto-allocate starting from 2222
        for port in 2222..=2399 {
            if is_port_free(port) {
                return Ok(port);
            }
        }

        bail!("Could not find an available host port in range 2222..=2399. Please specify a custom port with --port.");
    }

    /// Resolve and validate instance storage path.
    /// Ensures per-instance isolation when default path is used,
    /// and checks that no other running container is currently binding the same path.
    pub async fn resolve_and_validate_storage(
        &self,
        name: &str,
        custom_storage: Option<PathBuf>,
    ) -> Result<PathBuf> {
        let storage_path = match custom_storage {
            Some(path) => path,
            None => {
                // Backward compatibility: If legacy ./vmpod-data/disk.qcow2 exists and name is vmpod-box, use it.
                if name == "vmpod-box" && Path::new("./vmpod-data/disk.qcow2").exists() {
                    PathBuf::from("./vmpod-data")
                } else {
                    PathBuf::from(format!("./vmpod-data/{}", name))
                }
            }
        };

        // Ensure directory exists
        if !storage_path.exists() {
            tokio::fs::create_dir_all(&storage_path)
                .await
                .with_context(|| {
                    format!(
                        "Failed to create storage directory at '{}'",
                        storage_path.display()
                    )
                })?;
        }

        let canonical_storage = std::fs::canonicalize(&storage_path).with_context(|| {
            format!(
                "Failed to canonicalize storage path '{}'",
                storage_path.display()
            )
        })?;

        // Check if any active running container is already bound to this directory
        let canonical_str = canonical_storage.to_string_lossy();

        if let Ok(instances) = self.list_instances().await {
            for inst in instances {
                if inst.running && inst.name != name && inst.storage_path == canonical_str {
                    bail!(
                        "Storage path '{}' is already in active use by running VM '{}'.\n\
                         Concurrent access to the same virtual disk will cause QEMU locking failure and disk corruption.\n\
                         Please specify an isolated storage folder for '{}' with '--storage <PATH>'.",
                        canonical_storage.display(),
                        inst.name,
                        name
                    );
                }
            }
        }

        Ok(canonical_storage)
    }

    /// Spin up a hardware-accelerated Linux VM container
    pub async fn up(&self, params: UpParams) -> Result<UpResult> {
        // Step 1: Pre-flight check
        self.preflight_check().await?;

        // Step 2: Storage path preparation and isolation check
        let canonical_storage = self
            .resolve_and_validate_storage(&params.name, params.storage)
            .await?;

        // Step 3: Check for existing container with the same name
        match self
            .docker
            .inspect_container(&params.name, None::<InspectContainerOptions>)
            .await
        {
            Ok(inspect) => {
                let is_running = inspect
                    .state
                    .as_ref()
                    .and_then(|s| s.running)
                    .unwrap_or(false);
                let id = inspect.id.unwrap_or_default();
                let short_id = if id.len() >= 12 { &id[..12] } else { &id };

                if is_running {
                    bail!(
                        "Container '{}' is already running (ID: {}).\n\
                         Run 'vmpod status --name {}' to inspect or 'vmpod down --name {}' to tear it down first.",
                        params.name,
                        short_id,
                        params.name,
                        params.name
                    );
                } else {
                    println!(
                        "[*] Cleaning up stale stopped container '{}' (ID: {})...",
                        params.name, short_id
                    );
                    self.docker
                        .remove_container(
                            &params.name,
                            Some(RemoveContainerOptions {
                                force: true,
                                ..Default::default()
                            }),
                        )
                        .await
                        .context("Failed to remove previous stopped container")?;
                }
            }
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                // Container does not exist, proceed cleanly
            }
            Err(e) => {
                return Err(e).context("Error inspecting existing container status");
            }
        }

        // Step 4: Resolve host SSH port (auto-detect if omitted or occupied)
        let actual_port = self.find_available_port(params.port).await?;

        // Step 5: Construct container configuration
        let mut devices = vec![DeviceMapping {
            path_on_host: Some("/dev/kvm".to_string()),
            path_in_container: Some("/dev/kvm".to_string()),
            cgroup_permissions: Some("rwm".to_string()),
        }];

        if Path::new("/dev/net/tun").exists() {
            devices.push(DeviceMapping {
                path_on_host: Some("/dev/net/tun".to_string()),
                path_in_container: Some("/dev/net/tun".to_string()),
                cgroup_permissions: Some("rwm".to_string()),
            });
        }

        // Direct host volume bind to bypass Docker overlay2 filesystem
        let bind_spec = format!("{}:/storage:rw", canonical_storage.display());

        // Port mapping for guest SSH (port 22)
        let guest_ssh_key = "22/tcp".to_string();
        let mut port_bindings: PortMap = HashMap::new();
        port_bindings.insert(
            guest_ssh_key.clone(),
            Some(vec![PortBinding {
                host_ip: Some("0.0.0.0".to_string()),
                host_port: Some(actual_port.to_string()),
            }]),
        );

        let host_config = HostConfig {
            devices: Some(devices),
            cap_add: Some(vec!["NET_ADMIN".to_string()]),
            binds: Some(vec![bind_spec]),
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let uid = unsafe { getuid() };
        let gid = unsafe { getgid() };

        let env_vars = vec![
            format!("RAM={}", params.ram),
            format!("SMP={}", params.smp),
            "SSH_PORT=22".to_string(),
            format!("HOST_UID={}", uid),
            format!("HOST_GID={}", gid),
        ];

        // Attach standardized labels for multi-instance management and discovery
        let mut labels = HashMap::new();
        labels.insert(LABEL_MANAGED.to_string(), "true".to_string());
        labels.insert(LABEL_NAME.to_string(), params.name.clone());
        labels.insert(LABEL_SSH_PORT.to_string(), actual_port.to_string());
        labels.insert(LABEL_RAM.to_string(), params.ram.clone());
        labels.insert(LABEL_SMP.to_string(), params.smp.to_string());

        let container_config = ContainerCreateBody {
            image: Some(params.image.clone()),
            env: Some(env_vars),
            exposed_ports: Some(vec![guest_ssh_key]),
            labels: Some(labels),
            host_config: Some(host_config),
            ..Default::default()
        };

        // Step 6: Create and start container via Bollard
        let create_response = self
            .docker
            .create_container(
                Some(CreateContainerOptions {
                    name: Some(params.name.clone()),
                    platform: String::new(),
                }),
                container_config,
            )
            .await
            .with_context(|| {
                format!(
                    "Failed to create Docker container '{}' using image '{}'.\n\
                     Ensure the image is built: 'docker build -t {} .'",
                    params.name, params.image, params.image
                )
            })?;

        let container_id = create_response.id;
        let short_id = if container_id.len() >= 12 {
            container_id[..12].to_string()
        } else {
            container_id.clone()
        };

        self.docker
            .start_container(&params.name, None::<StartContainerOptions>)
            .await
            .with_context(|| {
                format!(
                    "Failed to start VM container '{}' (ID: {})",
                    params.name, short_id
                )
            })?;

        let ssh_command = format!("ssh root@localhost -p {}", actual_port);

        Ok(UpResult {
            container_id,
            short_id,
            name: params.name,
            port: actual_port,
            ssh_command,
            status: "running".to_string(),
            storage_path: canonical_storage,
        })
    }

    /// Gracefully stop the VM container with 5-second timeout and remove it
    pub async fn down(&self, name: &str) -> Result<()> {
        let inspect = match self
            .docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
        {
            Ok(res) => res,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                bail!("VM container '{}' not found.", name);
            }
            Err(e) => return Err(e).context("Error inspecting container"),
        };

        let is_running = inspect
            .state
            .as_ref()
            .and_then(|s| s.running)
            .unwrap_or(false);

        if is_running {
            println!(
                "[*] Gracefully stopping container '{}' (5-second timeout for QEMU disk flush)...",
                name
            );
            self.docker
                .stop_container(
                    name,
                    Some(StopContainerOptions {
                        signal: None,
                        t: Some(5),
                    }),
                )
                .await
                .with_context(|| format!("Failed to stop container '{name}'"))?;
            println!("[✓] Container '{}' stopped.", name);
        } else {
            println!("[*] Container '{}' is already stopped.", name);
        }

        println!("[*] Removing container '{}'...", name);
        self.docker
            .remove_container(
                name,
                Some(RemoveContainerOptions {
                    force: true,
                    ..Default::default()
                }),
            )
            .await
            .with_context(|| format!("Failed to remove container '{name}'"))?;

        println!("[✓] Container '{}' successfully removed.", name);
        Ok(())
    }

    /// Gracefully stop and remove all managed vmpod instances
    pub async fn down_all(&self) -> Result<Vec<String>> {
        let instances = self.list_instances().await?;
        if instances.is_empty() {
            println!("No vmpod instances currently found on host.");
            return Ok(Vec::new());
        }

        let mut removed = Vec::new();
        for inst in instances {
            println!("------------------------------------------------------------");
            match self.down(&inst.name).await {
                Ok(_) => removed.push(inst.name),
                Err(e) => eprintln!("[!] Failed to tear down '{}': {:#}", inst.name, e),
            }
        }
        Ok(removed)
    }

    /// List all managed vmpod instances on the host
    pub async fn list_instances(&self) -> Result<Vec<InstanceSummary>> {
        let mut filters = HashMap::new();
        filters.insert("label".to_string(), vec![format!("{}=true", LABEL_MANAGED)]);

        // Query labeled containers (or scan image matching vmpod/qemu-runner)
        let containers = self
            .docker
            .list_containers(Some(
                ListContainersOptionsBuilder::default().all(true).build(),
            ))
            .await
            .context("Failed to list Docker containers")?;

        let mut instances = Vec::new();

        for c in containers {
            let labels = c.labels.unwrap_or_default();
            let is_vmpod = labels.get(LABEL_MANAGED).map(|v| v == "true").unwrap_or(false)
                || c.image
                    .as_deref()
                    .map(|img| img.contains("vmpod/qemu-runner"))
                    .unwrap_or(false)
                || c.names
                    .as_ref()
                    .and_then(|n| n.first())
                    .map(|n| n.contains("vmpod"))
                    .unwrap_or(false);

            if !is_vmpod {
                continue;
            }

            let container_id = c.id.unwrap_or_default();
            let short_id = if container_id.len() >= 12 {
                container_id[..12].to_string()
            } else {
                container_id.clone()
            };

            // Raw name in Docker starts with '/'
            let raw_name = c
                .names
                .and_then(|names| names.into_iter().next())
                .unwrap_or_else(|| "unknown".to_string());
            let name = raw_name.trim_start_matches('/').to_string();

            let state_str = c.state.map(|s| s.to_string()).unwrap_or_default();
            let running = state_str == "running";
            let status = c.status.unwrap_or(state_str);

            let mut port = None;
            if let Some(ports) = c.ports {
                for p in ports {
                    if p.private_port == 22 {
                        port = p.public_port;
                        break;
                    }
                }
            }
            if port.is_none() {
                if let Some(p_str) = labels.get(LABEL_SSH_PORT) {
                    port = p_str.parse::<u16>().ok();
                }
            }

            let ssh_command = match port {
                Some(p) => format!("ssh root@localhost -p {p}"),
                None => "None".to_string(),
            };

            let ram = labels
                .get(LABEL_RAM)
                .cloned()
                .unwrap_or_else(|| "2G".to_string());
            let smp = labels
                .get(LABEL_SMP)
                .cloned()
                .unwrap_or_else(|| "2".to_string());

            let mut storage_path = "dynamic".to_string();
            if let Some(mounts) = c.mounts {
                for m in mounts {
                    if m.destination == Some("/storage".to_string()) {
                        if let Some(src) = m.source {
                            storage_path = src;
                            break;
                        }
                    }
                }
            }

            // Calculate uptime if created timestamp is present
            let uptime = if running {
                if let Some(created) = c.created {
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs() as i64;
                    if now >= created {
                        let diff = (now - created) as u64;
                        format_duration(diff)
                    } else {
                        "running".to_string()
                    }
                } else {
                    "running".to_string()
                }
            } else {
                "stopped".to_string()
            };

            instances.push(InstanceSummary {
                name,
                container_id,
                short_id,
                status,
                running,
                port,
                ssh_command,
                uptime,
                ram,
                smp,
                storage_path,
            });
        }

        instances.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(instances)
    }

    /// Inspect VM container state and format structured status
    pub async fn status(&self, name: &str) -> Result<StatusResult> {
        let inspect = match self
            .docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
        {
            Ok(res) => res,
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => {
                bail!("VM container '{}' does not exist.", name);
            }
            Err(e) => return Err(e).context("Failed to inspect container"),
        };

        let container_id = inspect.id.unwrap_or_else(|| "unknown".to_string());
        let short_id = if container_id.len() >= 12 {
            container_id[..12].to_string()
        } else {
            container_id.clone()
        };

        let state = inspect.state.unwrap_or_default();
        let running = state.running.unwrap_or(false);
        let status_str = state
            .status
            .map(|s| s.to_string())
            .unwrap_or_else(|| if running { "running".into() } else { "stopped".into() });

        let started_at_raw = state.started_at.unwrap_or_default();
        let uptime = if running && !started_at_raw.is_empty() {
            calculate_uptime(&started_at_raw)
        } else if running {
            "running".to_string()
        } else {
            "stopped".to_string()
        };

        // Extract port mappings and build SSH command
        let mut port_mappings = Vec::new();
        let mut primary_ssh_port: Option<u16> = None;

        if let Some(network_settings) = inspect.network_settings {
            if let Some(ports) = network_settings.ports {
                for (container_port, host_bindings) in ports {
                    if let Some(bindings) = host_bindings {
                        for b in bindings {
                            let host_ip = b.host_ip.unwrap_or_else(|| "0.0.0.0".to_string());
                            let host_port = b.host_port.unwrap_or_else(|| "0".to_string());
                            port_mappings.push(format!("{host_ip}:{host_port} -> {container_port}"));

                            if container_port.starts_with("22") {
                                if let Ok(p) = host_port.parse::<u16>() {
                                    primary_ssh_port = Some(p);
                                }
                            }
                        }
                    }
                }
            }
        }

        let ssh_command = primary_ssh_port.map(|p| format!("ssh root@localhost -p {p}"));

        // Extract storage host bind
        let storage_bind = inspect.host_config.and_then(|hc| hc.binds).and_then(|binds| {
            binds
                .into_iter()
                .find(|b| b.contains(":/storage"))
        });

        let config = inspect.config.unwrap_or_default();
        let image = config.image.unwrap_or_else(|| "unknown".to_string());

        let labels = config.labels.unwrap_or_default();
        let ram = labels.get(LABEL_RAM).cloned();
        let smp = labels.get(LABEL_SMP).cloned();

        Ok(StatusResult {
            container_id,
            short_id,
            name: name.to_string(),
            status: status_str,
            running,
            uptime,
            port_mappings,
            ssh_command,
            storage_bind,
            image,
            ram,
            smp,
        })
    }
}

fn format_duration(seconds: u64) -> String {
    let days = seconds / 86400;
    let hours = (seconds % 86400) / 3600;
    let mins = (seconds % 3600) / 60;
    let secs = seconds % 60;

    if days > 0 {
        format!("{days}d {hours}h {mins}m")
    } else if hours > 0 {
        format!("{hours}h {mins}m {secs}s")
    } else if mins > 0 {
        format!("{mins}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

/// Compute human-readable uptime from Docker's ISO 8601 timestamp string
fn calculate_uptime(iso8601: &str) -> String {
    let parse_epoch = |s: &str| -> Option<u64> {
        let clean = s.split('.').next()?.trim_end_matches('Z');
        let parts: Vec<&str> = clean.split('T').collect();
        if parts.len() != 2 {
            return None;
        }

        let ymd: Vec<u64> = parts[0].split('-').filter_map(|x| x.parse().ok()).collect();
        let hms: Vec<u64> = parts[1].split(':').filter_map(|x| x.parse().ok()).collect();
        if ymd.len() != 3 || hms.len() != 3 {
            return None;
        }

        let (year, month, day) = (ymd[0], ymd[1], ymd[2]);
        let (hour, min, sec) = (hms[0], hms[1], hms[2]);

        let mut total_days = 0u64;
        for y in 1970..year {
            let is_leap = (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0);
            total_days += if is_leap { 366 } else { 365 };
        }

        let is_leap_year = (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0);
        let days_in_month = [
            31,
            if is_leap_year { 29 } else { 28 },
            31, 30, 31, 30, 31, 31, 30, 31, 30, 31,
        ];

        for m in 1..month {
            total_days += days_in_month[(m - 1) as usize];
        }
        total_days += day.saturating_sub(1);

        Some(total_days * 86400 + hour * 3600 + min * 60 + sec)
    };

    if let Some(epoch_sec) = parse_epoch(iso8601) {
        if let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) {
            let now_sec = now.as_secs();
            if now_sec >= epoch_sec {
                return format_duration(now_sec - epoch_sec);
            }
        }
    }

    "running".to_string()
}
