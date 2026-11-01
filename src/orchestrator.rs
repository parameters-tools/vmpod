use anyhow::{bail, Context, Result};
use bollard::models::{
    ContainerCreateBody, DeviceMapping, HostConfig, PortBinding, PortMap,
};
use bollard::query_parameters::{
    CreateContainerOptions, InspectContainerOptions, ListContainersOptionsBuilder,
    RemoveContainerOptions, StartContainerOptions, StopContainerOptions,
};
use bollard::Docker;
use serde::{Deserialize, Serialize};
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
pub const LABEL_EPHEMERAL: &str = "io.vmpod.ephemeral";

pub const MAX_GUARDRAIL_SMP: u32 = 32;
pub const MAX_GUARDRAIL_RAM_GB: u64 = 64;
pub const MAX_GUARDRAIL_TIMEOUT_SECS: u64 = 1800;

pub fn validate_name(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '-');
    if !valid {
        bail!(
            "Invalid instance name '{}': must be 1-64 characters and contain only [a-zA-Z0-9_.-].",
            name
        );
    }
    Ok(())
}

pub fn validate_smp(smp: u32) -> Result<()> {
    if smp == 0 {
        bail!("Invalid vCPU count: --smp must be at least 1 (got 0).");
    }
    if smp > MAX_GUARDRAIL_SMP {
        bail!(
            "Guardrail violation: requested {} vCPUs exceeds host safety limit of {} vCPUs.",
            smp,
            MAX_GUARDRAIL_SMP
        );
    }
    Ok(())
}

pub fn validate_ram(ram: &str) -> Result<()> {
    let ram_trim = ram.trim();
    let (num_part, unit) = ram_trim.split_at(ram_trim.len().saturating_sub(1));
    let ram_val_gb = if unit.eq_ignore_ascii_case("g") {
        num_part.parse::<u64>().ok()
    } else if unit.eq_ignore_ascii_case("m") {
        num_part.parse::<u64>().ok().map(|m| (m + 1023) / 1024)
    } else {
        ram_trim.parse::<u64>().ok()
    };

    match ram_val_gb {
        Some(n) if n > 0 && n <= MAX_GUARDRAIL_RAM_GB => Ok(()),
        Some(n) if n > MAX_GUARDRAIL_RAM_GB => {
            bail!(
                "Guardrail violation: requested RAM '{}' exceeds host safety limit of {}GB.",
                ram,
                MAX_GUARDRAIL_RAM_GB
            );
        }
        _ => {
            bail!(
                "Invalid RAM allocation '{}': expected a positive memory size up to {}GB (e.g. '512M', '2G', '4G').",
                ram,
                MAX_GUARDRAIL_RAM_GB
            );
        }
    }
}

pub fn validate_timeout(timeout_secs: u64) -> Result<()> {
    if timeout_secs > MAX_GUARDRAIL_TIMEOUT_SECS {
        bail!(
            "Guardrail violation: timeout {}s exceeds max safety limit of {}s.",
            timeout_secs,
            MAX_GUARDRAIL_TIMEOUT_SECS
        );
    }
    Ok(())
}

/// Parameters for spinning up a new VM container instance
#[derive(Debug, Clone)]
pub struct UpParams {
    pub name: String,
    pub port: Option<u16>,
    pub ram: String,
    pub smp: u32,
    pub storage: Option<PathBuf>,
    pub image: String,
    pub ephemeral: bool,
}

/// Result returned after a successful `up` operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpResult {
    pub container_id: String,
    pub short_id: String,
    pub name: String,
    pub port: u16,
    pub ssh_command: String,
    pub status: String,
    pub storage_path: PathBuf,
}

/// Result returned after a successful `spin` operation (booted & verified ready)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpinResult {
    pub status: String,
    pub name: String,
    pub port: u16,
    pub boot_time_ms: u128,
    pub storage: PathBuf,
    pub ephemeral: bool,
    pub ssh_command: String,
}

/// Result of executing a command inside a VM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunResult {
    pub instance: String,
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub timed_out: bool,
}

/// Result of tearing down a VM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DownResult {
    pub status: String,
    pub name: String,
    pub purged: bool,
}

/// Result returned after inspecting a VM container
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub ephemeral: bool,
}

/// Summary of an instance for listing
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    pub ephemeral: bool,
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

        // Step 1b: Input validations & guardrails
        validate_smp(params.smp)?;
        validate_name(&params.name)?;
        validate_ram(&params.ram)?;

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
        if params.ephemeral {
            labels.insert(LABEL_EPHEMERAL.to_string(), "true".to_string());
        }

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

        // Step 7: Post-start health check (verify container didn't crash on boot)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let post_inspect = self
            .docker
            .inspect_container(&params.name, None::<InspectContainerOptions>)
            .await?;

        if let Some(state) = post_inspect.state {
            let running = state.running.unwrap_or(false);
            let exit_code = state.exit_code.unwrap_or(0);
            if !running || exit_code != 0 {
                let logs = self.get_container_logs(&params.name, 30).await.unwrap_or_default();
                let _ = self.docker.remove_container(
                    &params.name,
                    Some(RemoveContainerOptions { force: true, ..Default::default() }),
                ).await;
                bail!(
                    "VM container '{}' failed during boot (exit code: {}).\n\
                     Runner diagnostics:\n{}",
                    params.name,
                    exit_code,
                    logs.trim()
                );
            }
        }

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

    /// Fetch recent stdout/stderr output from a container
    pub async fn get_container_logs(&self, name: &str, tail_lines: u32) -> Result<String> {
        use futures_util::StreamExt;
        let mut stream = self.docker.logs(
            name,
            Some(bollard::query_parameters::LogsOptions {
                stdout: true,
                stderr: true,
                tail: tail_lines.to_string(),
                ..Default::default()
            }),
        );

        let mut output = String::new();
        while let Some(chunk) = stream.next().await {
            if let Ok(msg) = chunk {
                output.push_str(&msg.to_string());
            }
        }
        Ok(output)
    }

    /// Wait for VM guest SSH daemon to accept connections and authenticate
    pub async fn wait_for_ssh_ready(
        &self,
        port: u16,
        key_path: &Path,
        timeout: std::time::Duration,
    ) -> Result<std::time::Duration> {
        let start = std::time::Instant::now();
        let interval = std::time::Duration::from_millis(500);

        while start.elapsed() < timeout {
            if let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
                let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;

                let output = tokio::process::Command::new("ssh")
                    .arg("-n")
                    .arg("-i").arg(key_path)
                    .arg("-o").arg("BatchMode=yes")
                    .arg("-o").arg("StrictHostKeyChecking=no")
                    .arg("-o").arg("UserKnownHostsFile=/dev/null")
                    .arg("-o").arg("LogLevel=ERROR")
                    .arg("-o").arg("ConnectTimeout=2")
                    .arg("-p").arg(port.to_string())
                    .arg("root@127.0.0.1")
                    .arg("true")
                    .stdin(std::process::Stdio::null())
                    .output()
                    .await;

                if let Ok(out) = output {
                    if out.status.success() {
                        return Ok(start.elapsed());
                    }
                }
            }

            tokio::time::sleep(interval).await;
        }

        bail!(
            "Timeout waiting for VM SSH readiness on port {} after {}s.",
            port,
            timeout.as_secs()
        );
    }

    /// Spin up a VM and block until the guest OS and SSH are 100% ready to execute commands
    pub async fn spin(
        &self,
        params: UpParams,
        readiness_timeout: std::time::Duration,
    ) -> Result<SpinResult> {
        let name = params.name.clone();
        let ephemeral = params.ephemeral;
        let up_res = self.up(params).await?;

        let key_path = up_res.storage_path.join("id_ed25519");
        let boot_duration = match self
            .wait_for_ssh_ready(up_res.port, &key_path, readiness_timeout)
            .await
        {
            Ok(dur) => dur,
            Err(e) => {
                if ephemeral {
                    let _ = self.down(&name, true).await;
                }
                return Err(e);
            }
        };

        Ok(SpinResult {
            status: "ready".to_string(),
            name: up_res.name,
            port: up_res.port,
            boot_time_ms: boot_duration.as_millis(),
            storage: up_res.storage_path,
            ephemeral,
            ssh_command: up_res.ssh_command,
        })
    }

    /// Execute a command non-interactively inside the VM over SSH with timeout protection
    pub async fn run_command(
        &self,
        name: &str,
        command: &str,
        workdir: Option<&str>,
        envs: &[String],
        timeout: std::time::Duration,
    ) -> Result<RunResult> {
        validate_timeout(timeout.as_secs())?;

        let instances = self.list_instances().await?;
        let inst = instances
            .iter()
            .find(|i| i.name == name)
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' not found.", name))?;

        if !inst.running {
            bail!(
                "VM instance '{}' is not currently running. Run 'vmpod up {}' or 'vmpod spin {}' first.",
                name,
                name,
                name
            );
        }

        let port = inst
            .port
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' has no SSH port mapping.", name))?;
        let key_path = PathBuf::from(&inst.storage_path).join("id_ed25519");

        // Format command with environment variables and workdir if requested
        let mut script = String::new();
        for env in envs {
            if let Some((k, v)) = env.split_once('=') {
                script.push_str(&format!("export {}='{}' && ", k, v.replace('\'', "'\\''")));
            }
        }
        if let Some(wd) = workdir {
            script.push_str(&format!("mkdir -p '{}' && cd '{}' && ", wd, wd));
        }
        script.push_str(command);

        let start = std::time::Instant::now();

        let mut child = tokio::process::Command::new("ssh")
            .arg("-n")
            .arg("-i").arg(&key_path)
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("StrictHostKeyChecking=no")
            .arg("-o").arg("UserKnownHostsFile=/dev/null")
            .arg("-o").arg("LogLevel=ERROR")
            .arg("-o").arg("ConnectTimeout=5")
            .arg("-p").arg(port.to_string())
            .arg("root@127.0.0.1")
            .arg(&script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn ssh process")?;

        let mut stdout_pipe = child.stdout.take().context("Failed to open child stdout")?;
        let mut stderr_pipe = child.stderr.take().context("Failed to open child stderr")?;
        let mut stdout_bytes = Vec::new();
        let mut stderr_bytes = Vec::new();

        let timeout_result = tokio::time::timeout(timeout, async {
            let (out_res, err_res, status_res) = tokio::join!(
                tokio::io::AsyncReadExt::read_to_end(&mut stdout_pipe, &mut stdout_bytes),
                tokio::io::AsyncReadExt::read_to_end(&mut stderr_pipe, &mut stderr_bytes),
                child.wait()
            );
            match (out_res, err_res, status_res) {
                (Ok(_), Ok(_), Ok(status)) => Ok(status),
                (Err(e), _, _) | (_, Err(e), _) => Err(anyhow::anyhow!(e)),
                (_, _, Err(e)) => Err(anyhow::anyhow!(e)),
            }
        }).await;
        let duration = start.elapsed();

        match timeout_result {
            Ok(Ok(status)) => {
                let stdout = String::from_utf8_lossy(&stdout_bytes).to_string();
                let stderr = String::from_utf8_lossy(&stderr_bytes).to_string();
                let exit_code = status.code().unwrap_or(-1);

                Ok(RunResult {
                    instance: name.to_string(),
                    command: command.to_string(),
                    exit_code,
                    stdout,
                    stderr,
                    duration_ms: duration.as_millis(),
                    timed_out: false,
                })
            }
            Ok(Err(e)) => Err(e).context("Failed waiting for command execution"),
            Err(_) => {
                let _ = child.start_kill();
                Ok(RunResult {
                    instance: name.to_string(),
                    command: command.to_string(),
                    exit_code: 124,
                    stdout: String::new(),
                    stderr: format!("Command timed out after {}s", timeout.as_secs()),
                    duration_ms: duration.as_millis(),
                    timed_out: true,
                })
            }
        }
    }

    /// Write file contents directly to a path inside the VM via SSH stdin.
    pub async fn write_file(&self, name: &str, path: &str, content: &[u8]) -> Result<()> {
        let instances = self.list_instances().await?;
        let inst = instances
            .iter()
            .find(|i| i.name == name)
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' not found.", name))?;

        if !inst.running {
            bail!("VM instance '{}' is not currently running.", name);
        }

        let port = inst
            .port
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' has no SSH port mapping.", name))?;
        let key_path = PathBuf::from(&inst.storage_path).join("id_ed25519");

        let escaped_path = path.replace('\'', "'\\''");
        let shell_cmd = format!("mkdir -p \"$(dirname '{}')\" && cat > '{}'", escaped_path, escaped_path);

        let mut child = tokio::process::Command::new("ssh")
            .arg("-i").arg(&key_path)
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("StrictHostKeyChecking=no")
            .arg("-o").arg("UserKnownHostsFile=/dev/null")
            .arg("-o").arg("LogLevel=ERROR")
            .arg("-o").arg("ConnectTimeout=5")
            .arg("-p").arg(port.to_string())
            .arg("root@127.0.0.1")
            .arg(&shell_cmd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .context("Failed to spawn ssh for write_file")?;

        if let Some(mut stdin) = child.stdin.take() {
            tokio::io::AsyncWriteExt::write_all(&mut stdin, content)
                .await
                .context("Failed to write content to ssh stdin")?;
            tokio::io::AsyncWriteExt::flush(&mut stdin)
                .await
                .context("Failed to flush ssh stdin")?;
            drop(stdin);
        }

        let output = child
            .wait_with_output()
            .await
            .context("Failed to wait for write_file completion")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to write file inside VM: {}", err.trim());
        }

        Ok(())
    }

    /// Read file contents from inside the VM via SSH.
    pub async fn read_file(&self, name: &str, path: &str) -> Result<String> {
        let instances = self.list_instances().await?;
        let inst = instances
            .iter()
            .find(|i| i.name == name)
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' not found.", name))?;

        if !inst.running {
            bail!("VM instance '{}' is not currently running.", name);
        }

        let port = inst
            .port
            .ok_or_else(|| anyhow::anyhow!("VM instance '{}' has no SSH port mapping.", name))?;
        let key_path = PathBuf::from(&inst.storage_path).join("id_ed25519");

        let escaped_path = path.replace('\'', "'\\''");
        let shell_cmd = format!("cat '{}'", escaped_path);

        let output = tokio::process::Command::new("ssh")
            .arg("-n")
            .arg("-i").arg(&key_path)
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("StrictHostKeyChecking=no")
            .arg("-o").arg("UserKnownHostsFile=/dev/null")
            .arg("-o").arg("LogLevel=ERROR")
            .arg("-o").arg("ConnectTimeout=5")
            .arg("-p").arg(port.to_string())
            .arg("root@127.0.0.1")
            .arg(&shell_cmd)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .context("Failed to execute ssh for read_file")?;

        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to read file '{}': {}", path, err.trim());
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Gracefully stop the VM container and remove it.
    /// If `purge` is true or if the container is labeled ephemeral, deletes host storage.
    pub async fn down(&self, name: &str, purge: bool) -> Result<DownResult> {
        let inspect = match self
            .docker
            .inspect_container(name, None::<InspectContainerOptions>)
            .await
        {
            Ok(res) => Some(res),
            Err(bollard::errors::Error::DockerResponseServerError {
                status_code: 404, ..
            }) => None,
            Err(e) => return Err(e).context("Error inspecting container"),
        };

        let mut is_ephemeral = false;
        let mut storage_path_to_purge: Option<PathBuf> = None;

        if let Some(ref c) = inspect {
            is_ephemeral = c
                .config
                .as_ref()
                .and_then(|cfg| cfg.labels.as_ref())
                .and_then(|l| l.get(LABEL_EPHEMERAL))
                .map(|v| v == "true")
                .unwrap_or(false);

            if let Some(mounts) = c.mounts.as_ref() {
                for m in mounts {
                    if m.destination == Some("/storage".to_string()) {
                        if let Some(src) = m.source.as_ref() {
                            storage_path_to_purge = Some(PathBuf::from(src));
                            break;
                        }
                    }
                }
            }

            let is_running = c
                .state
                .as_ref()
                .and_then(|s| s.running)
                .unwrap_or(false);

            if is_running {
                if let Err(e) = self
                    .docker
                    .stop_container(
                        name,
                        Some(StopContainerOptions {
                            signal: None,
                            t: Some(5),
                        }),
                    )
                    .await
                {
                    match &e {
                        bollard::errors::Error::DockerResponseServerError {
                            status_code: 304, ..
                        } => {}
                        _ => return Err(e).context(format!("Failed to stop container '{name}'")),
                    }
                }
            }

            if let Err(e) = self
                .docker
                .remove_container(
                    name,
                    Some(RemoveContainerOptions {
                        force: true,
                        ..Default::default()
                    }),
                )
                .await
            {
                match &e {
                    bollard::errors::Error::DockerResponseServerError {
                        status_code: 404, ..
                    } => {}
                    bollard::errors::Error::DockerResponseServerError {
                        status_code: 409,
                        message,
                    } if message.contains("removal") && message.contains("in progress") => {}
                    _ => return Err(e).context(format!("Failed to remove container '{name}'")),
                }
            }
        }

        if storage_path_to_purge.is_none() {
            let fallback = PathBuf::from("./vmpod-data").join(name);
            if fallback.exists() {
                storage_path_to_purge = Some(fallback);
            }
        }

        let mut actually_purged = false;
        if (purge || is_ephemeral) && storage_path_to_purge.is_some() {
            let p = storage_path_to_purge.unwrap();
            if p.exists() {
                let _ = std::fs::remove_dir_all(&p);
                actually_purged = true;
            }
        } else if inspect.is_none() && !purge {
            bail!("VM container '{}' not found.", name);
        }

        Ok(DownResult {
            status: "terminated".to_string(),
            name: name.to_string(),
            purged: actually_purged,
        })
    }

    /// Gracefully stop and remove all managed vmpod instances
    pub async fn down_all(&self, purge: bool) -> Result<Vec<DownResult>> {
        let instances = self.list_instances().await?;
        if instances.is_empty() {
            return Ok(Vec::new());
        }

        let mut removed = Vec::new();
        for inst in instances {
            if let Ok(res) = self.down(&inst.name, purge).await {
                removed.push(res);
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
                || c.names
                    .as_ref()
                    .and_then(|n| n.first())
                    .map(|n| n.trim_start_matches('/') == "vmpod-box")
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

            let ephemeral = labels
                .get(LABEL_EPHEMERAL)
                .map(|v| v == "true")
                .unwrap_or(false);

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
                ephemeral,
            });
        }

        // Discover offline persistent instances with existing virtual disks in ./vmpod-data
        let vmpod_data = PathBuf::from("./vmpod-data");
        if let Ok(entries) = std::fs::read_dir(&vmpod_data) {
            for entry in entries.flatten() {
                if let Ok(file_type) = entry.file_type() {
                    if file_type.is_dir() {
                        let dir_name = entry.file_name().to_string_lossy().to_string();
                        let disk = entry.path().join("disk.qcow2");
                        if disk.exists() && !instances.iter().any(|i| i.name == dir_name) {
                            let canonical = entry.path().canonicalize().unwrap_or(entry.path());
                            instances.push(InstanceSummary {
                                name: dir_name.clone(),
                                container_id: "offline".to_string(),
                                short_id: "offline".to_string(),
                                status: "stopped".to_string(),
                                running: false,
                                port: None,
                                ssh_command: format!("vmpod up {}", dir_name),
                                uptime: "offline".to_string(),
                                ram: "-".to_string(),
                                smp: "-".to_string(),
                                storage_path: canonical.to_string_lossy().to_string(),
                                ephemeral: false,
                            });
                        }
                    }
                }
            }
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
        let ephemeral = labels
            .get(LABEL_EPHEMERAL)
            .map(|v| v == "true")
            .unwrap_or(false);

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
            ephemeral,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_name() {
        assert!(validate_name("agent-1").is_ok());
        assert!(validate_name("test_vm.01").is_ok());
        assert!(validate_name("vmpod-box").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("name with spaces").is_err());
        assert!(validate_name("bad/slash").is_err());
        assert!(validate_name("bad:colon").is_err());
        assert!(validate_name(&"a".repeat(65)).is_err());
        assert!(validate_name(&"a".repeat(64)).is_ok());
    }

    #[test]
    fn test_validate_smp() {
        assert!(validate_smp(1).is_ok());
        assert!(validate_smp(2).is_ok());
        assert!(validate_smp(32).is_ok());
        assert!(validate_smp(0).is_err());
        assert!(validate_smp(33).is_err());
        assert!(validate_smp(64).is_err());
    }

    #[test]
    fn test_validate_ram() {
        assert!(validate_ram("512M").is_ok());
        assert!(validate_ram("1G").is_ok());
        assert!(validate_ram("2g").is_ok());
        assert!(validate_ram("64G").is_ok());
        assert!(validate_ram("0G").is_err());
        assert!(validate_ram("65G").is_err());
        assert!(validate_ram("128G").is_err());
        assert!(validate_ram("not_a_number").is_err());
    }

    #[test]
    fn test_validate_timeout() {
        assert!(validate_timeout(1).is_ok());
        assert!(validate_timeout(60).is_ok());
        assert!(validate_timeout(1800).is_ok());
        assert!(validate_timeout(1801).is_err());
        assert!(validate_timeout(3600).is_err());
    }

    #[test]
    fn test_serde_spin_result() {
        let res = SpinResult {
            status: "ready".into(),
            name: "agent-1".into(),
            port: 2222,
            boot_time_ms: 8500,
            storage: PathBuf::from("./vmpod-data/agent-1"),
            ephemeral: true,
            ssh_command: "ssh root@127.0.0.1 -p 2222".into(),
        };
        let json_str = serde_json::to_string(&res).expect("serialization failed");
        assert!(json_str.contains("\"status\":\"ready\""));
        assert!(json_str.contains("\"ephemeral\":true"));
        assert!(json_str.contains("\"boot_time_ms\":8500"));

        let de: SpinResult = serde_json::from_str(&json_str).expect("deserialization failed");
        assert_eq!(de.name, "agent-1");
        assert_eq!(de.boot_time_ms, 8500);
        assert!(de.ephemeral);
    }

    #[test]
    fn test_serde_run_result() {
        let res = RunResult {
            instance: "agent-1".into(),
            command: "uname -a".into(),
            exit_code: 0,
            stdout: "Linux vmpod 6.6.0\n".into(),
            stderr: String::new(),
            duration_ms: 120,
            timed_out: false,
        };
        let json_str = serde_json::to_string(&res).expect("serialization failed");
        assert!(json_str.contains("\"exit_code\":0"));
        assert!(json_str.contains("\"timed_out\":false"));

        let de: RunResult = serde_json::from_str(&json_str).expect("deserialization failed");
        assert_eq!(de.exit_code, 0);
        assert_eq!(de.instance, "agent-1");
    }

    #[test]
    fn test_serde_down_result() {
        let res = DownResult {
            status: "stopped".into(),
            name: "agent-1".into(),
            purged: true,
        };
        let json_str = serde_json::to_string(&res).expect("serialization failed");
        assert!(json_str.contains("\"purged\":true"));
        assert!(json_str.contains("\"status\":\"stopped\""));

        let de: DownResult = serde_json::from_str(&json_str).expect("deserialization failed");
        assert_eq!(de.name, "agent-1");
        assert!(de.purged);
    }
}
