use anyhow::{bail, Context, Result};
use bollard::models::{ContainerCreateBody, DeviceMapping, HostConfig, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptions, InspectContainerOptions, RemoveContainerOptions,
    StartContainerOptions, StopContainerOptions,
};
use bollard::Docker;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Parameters for spinning up a new VM container instance
pub struct UpParams {
    pub name: String,
    pub port: u16,
    pub ram: String,
    pub smp: u32,
    pub storage: PathBuf,
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
             And ensure your user belongs to the 'docker' group: 'sudo usermod -aG docker $USER'"
        )?;

        Ok(())
    }

    /// Spin up a hardware-accelerated Linux VM container
    pub async fn up(&self, params: UpParams) -> Result<UpResult> {
        // Step 1: Pre-flight check
        self.preflight_check().await?;

        // Step 2: Storage path preparation and canonicalization
        if !params.storage.exists() {
            tokio::fs::create_dir_all(&params.storage)
                .await
                .with_context(|| {
                    format!(
                        "Failed to create storage directory at '{}'",
                        params.storage.display()
                    )
                })?;
        }
        let canonical_storage = std::fs::canonicalize(&params.storage).with_context(|| {
            format!(
                "Failed to canonicalize storage path '{}'",
                params.storage.display()
            )
        })?;

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

        // Step 4: Construct container configuration
        // Device mappings: /dev/kvm and /dev/net/tun with rwm permissions
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
                host_port: Some(params.port.to_string()),
            }]),
        );

        let host_config = HostConfig {
            devices: Some(devices),
            cap_add: Some(vec!["NET_ADMIN".to_string()]),
            binds: Some(vec![bind_spec]),
            port_bindings: Some(port_bindings),
            ..Default::default()
        };

        let env_vars = vec![
            format!("RAM={}", params.ram),
            format!("SMP={}", params.smp),
            "SSH_PORT=22".to_string(),
        ];

        let container_config = ContainerCreateBody {
            image: Some(params.image.clone()),
            env: Some(env_vars),
            exposed_ports: Some(vec![guest_ssh_key]),
            host_config: Some(host_config),
            ..Default::default()
        };

        // Step 5: Create and start container via Bollard
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

        let ssh_command = format!("ssh root@localhost -p {}", params.port);

        Ok(UpResult {
            container_id,
            short_id,
            name: params.name,
            port: params.port,
            ssh_command,
            status: "running".to_string(),
            storage_path: canonical_storage,
        })
    }

    /// Gracefully stop the VM container with 5-second timeout and remove it
    pub async fn down(&self, name: &str) -> Result<()> {
        // Step 1: Check if container exists
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

        // Step 2: Gracefully stop with 5-second timeout
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

        // Step 3: Remove container
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

        let image = inspect
            .config
            .and_then(|c| c.image)
            .unwrap_or_else(|| "unknown".to_string());

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
        })
    }
}

/// Compute human-readable uptime from Docker's ISO 8601 timestamp string
fn calculate_uptime(iso8601: &str) -> String {
    let parse_epoch = |s: &str| -> Option<u64> {
        // Expected format: YYYY-MM-DDTHH:MM:SS(...)Z
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

        // Days since Unix epoch (1970-01-01)
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
                let diff = now_sec - epoch_sec;
                let days = diff / 86400;
                let hours = (diff % 86400) / 3600;
                let mins = (diff % 3600) / 60;
                let secs = diff % 60;

                if days > 0 {
                    return format!("{days}d {hours}h {mins}m");
                } else if hours > 0 {
                    return format!("{hours}h {mins}m {secs}s");
                } else if mins > 0 {
                    return format!("{mins}m {secs}s");
                } else {
                    return format!("{secs}s");
                }
            }
        }
    }

    "running".to_string()
}
