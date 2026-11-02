mod mcp;
mod orchestrator;
mod tui;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use orchestrator::{UpParams, VmOrchestrator};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vmpod",
    author = "vmpod contributors",
    version = "1.5.0",
    about = "Fast, local-first CLI to spin up and orchestrate hardware-accelerated Linux VMs inside Docker using QEMU/KVM",
    long_about = "vmpod allows you to launch lightweight, hardware-accelerated virtual machines inside Docker containers \
                  with direct KVM pass-through, user-space networking, automatic root credentials provisioning, \
                  multi-instance orchestration, interactive TUI, agentic spin/run/down sandboxing, native Model Context Protocol (MCP) support, \
                  and instant QCOW2 point-in-time snapshot & rollback."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Spin up a VM and wait until guest OS & SSH are fully ready (agent-first)
    Spin(SpinArgs),
    /// Execute a command inside a running VM and return output & exit code (agent-first)
    Run(RunArgs),
    /// Launch the Model Context Protocol (MCP) JSON-RPC server on stdio (agent-first)
    Mcp,
    /// Manage point-in-time snapshots of virtual disks
    #[command(subcommand)]
    Snapshot(SnapshotCommands),
    /// Revert a VM back to a named snapshot
    Rollback(RollbackArgs),
    /// Spin up hardware-accelerated virtual machine container(s)
    Up(UpArgs),
    /// Gracefully stop and remove virtual machine container(s)
    Down(DownArgs),
    /// Inspect the status and health of a virtual machine container
    Status(StatusArgs),
    /// List all managed virtual machine instances
    #[command(alias = "ls", alias = "ps")]
    List(ListArgs),
    /// Launch the interactive terminal dashboard
    #[command(alias = "tui", alias = "ui")]
    Dashboard,
}

#[derive(Args, Debug)]
struct SpinArgs {
    /// Name for the VM container (positional or auto-generated if omitted)
    #[arg(value_name = "NAME")]
    name_pos: Option<String>,

    /// Unique identifier for the VM container (flag)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Host port mapped to the VM's SSH port (auto-allocated if omitted or in use)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,

    /// Guest RAM allocation (e.g. 512M, 2G, 4G)
    #[arg(long, default_value = "2G", value_name = "RAM")]
    ram: String,

    /// Guest vCPU core count (1-32)
    #[arg(long, default_value_t = 2, value_name = "CPUS")]
    smp: u32,

    /// Host directory where virtual disk images live (defaults to ./vmpod-data/<NAME>)
    #[arg(long, value_name = "PATH")]
    storage: Option<PathBuf>,

    /// Docker image running QEMU
    #[arg(long, default_value = "vmpod/qemu-runner:latest", value_name = "IMAGE")]
    image: String,

    /// Maximum time in seconds to wait for guest SSH readiness (default: 60)
    #[arg(short, long, default_value_t = 60, value_name = "SECONDS")]
    timeout: u64,

    /// Automatically purge virtual disk and storage when the VM is shut down
    #[arg(short, long)]
    ephemeral: bool,

    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Name of the running VM instance
    #[arg(value_name = "INSTANCE")]
    name: String,

    /// Command to execute inside the VM
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    command: Vec<String>,

    /// Execution timeout in seconds (default: 60, max: 1800)
    #[arg(short, long, default_value_t = 60, value_name = "SECONDS")]
    timeout: u64,

    /// Working directory inside the guest VM
    #[arg(short, long, value_name = "PATH")]
    workdir: Option<String>,

    /// Environment variables to set in the guest VM (KEY=VALUE)
    #[arg(short, long = "env", value_name = "KEY=VALUE")]
    envs: Vec<String>,

    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct UpArgs {
    /// Name or identifier for the VM container (positional)
    #[arg(value_name = "NAME")]
    name_pos: Option<String>,

    /// Unique identifier for the VM container (flag)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Number of VM instances to spin up (default: 1)
    #[arg(short, long, default_value_t = 1, value_name = "COUNT")]
    count: u32,

    /// Host port mapped to the VM's SSH port 22 (auto-allocated if omitted or in use)
    #[arg(long, value_name = "PORT")]
    port: Option<u16>,

    /// Guest RAM allocation (passed as environment variable to runner)
    #[arg(long, default_value = "2G", value_name = "RAM")]
    ram: String,

    /// Guest vCPU core count (passed as environment variable to runner)
    #[arg(long, default_value_t = 2, value_name = "CPUS")]
    smp: u32,

    /// Host directory where virtual disk images live (defaults to ./vmpod-data/<NAME>)
    #[arg(long, value_name = "PATH")]
    storage: Option<PathBuf>,

    /// Docker image running QEMU
    #[arg(long, default_value = "vmpod/qemu-runner:latest", value_name = "IMAGE")]
    image: String,

    /// Automatically purge virtual disk when stopped
    #[arg(short, long)]
    ephemeral: bool,

    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct DownArgs {
    /// Names of VM instances to tear down
    #[arg(value_name = "NAMES")]
    names: Vec<String>,

    /// Name of the VM instance to tear down (alternative to positional argument)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Stop and tear down all managed vmpod instances on host
    #[arg(short, long)]
    all: bool,

    /// Purge and permanently delete virtual disk and storage from host
    #[arg(short, long)]
    purge: bool,

    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct ListArgs {
    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Name of the VM instance to inspect (positional)
    #[arg(value_name = "NAME")]
    name_pos: Option<String>,

    /// Name of the VM instance to inspect (flag)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,

    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum SnapshotCommands {
    /// Create a new point-in-time snapshot of a VM's virtual disk
    Create(SnapshotCreateArgs),
    /// List all snapshots on a VM's virtual disk
    #[command(alias = "ls")]
    List(SnapshotListArgs),
    /// Revert a VM back to a named snapshot
    Rollback(RollbackArgs),
    /// Delete a named snapshot from a VM's virtual disk
    #[command(alias = "rm")]
    Delete(SnapshotDeleteArgs),
}

#[derive(Args, Debug)]
struct SnapshotCreateArgs {
    /// Target VM instance name
    name: String,
    /// Name or tag for the snapshot
    tag: String,
    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct SnapshotListArgs {
    /// Target VM instance name
    name: String,
    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct RollbackArgs {
    /// Target VM instance name
    name: String,
    /// Name or tag of the snapshot to revert to
    tag: String,
    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[derive(Args, Debug)]
struct SnapshotDeleteArgs {
    /// Target VM instance name
    name: String,
    /// Name or tag of the snapshot to delete
    tag: String,
    /// Output structured JSON
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let orchestrator = std::sync::Arc::new(VmOrchestrator::new()?);

    let command = cli.command.unwrap_or(Commands::Dashboard);

    match command {
        Commands::Dashboard => {
            tui::run_dashboard(orchestrator).await?;
        }

        Commands::Mcp => {
            mcp::run_mcp_server(orchestrator).await?;
        }

        Commands::Rollback(args) => {
            if !args.json {
                println!("============================================================");
                println!(" Rolling Back VM: {} -> '{}'", args.name, args.tag);
                println!("============================================================");
                println!("[*] Reverting virtual disk to checkpoint '{}'...", args.tag);
            }

            let res = orchestrator.snapshot_rollback(&args.name, &args.tag).await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&res)?);
            } else {
                println!("[✓] VM '{}' successfully reverted to snapshot '{}'!", res.instance, res.snapshot);
            }
        }

        Commands::Snapshot(subcmd) => match subcmd {
            SnapshotCommands::Create(args) => {
                if !args.json {
                    println!("============================================================");
                    println!(" Creating Snapshot for VM: {}", args.name);
                    println!("============================================================");
                    println!("[*] Checkpoint tag: '{}'", args.tag);
                }

                let res = orchestrator.snapshot_create(&args.name, &args.tag).await?;

                if args.json {
                    println!("{}", serde_json::to_string_pretty(&res)?);
                } else {
                    println!("[✓] Snapshot '{}' created successfully for VM '{}'!", res.snapshot, res.instance);
                }
            }

            SnapshotCommands::List(args) => {
                let snapshots = orchestrator.snapshot_list(&args.name).await?;

                if args.json {
                    println!("{}", serde_json::to_string_pretty(&snapshots)?);
                } else {
                    print_snapshots_table(&args.name, &snapshots);
                }
            }

            SnapshotCommands::Rollback(args) => {
                if !args.json {
                    println!("============================================================");
                    println!(" Rolling Back VM: {} -> '{}'", args.name, args.tag);
                    println!("============================================================");
                    println!("[*] Reverting virtual disk to checkpoint '{}'...", args.tag);
                }

                let res = orchestrator.snapshot_rollback(&args.name, &args.tag).await?;

                if args.json {
                    println!("{}", serde_json::to_string_pretty(&res)?);
                } else {
                    println!("[✓] VM '{}' successfully reverted to snapshot '{}'!", res.instance, res.snapshot);
                }
            }

            SnapshotCommands::Delete(args) => {
                let res = orchestrator.snapshot_delete(&args.name, &args.tag).await?;

                if args.json {
                    println!("{}", serde_json::to_string_pretty(&res)?);
                } else {
                    println!("[✓] Snapshot '{}' deleted from VM '{}'.", res.snapshot, res.instance);
                }
            }
        },

        Commands::Spin(args) => {
            let instance_name = args.name.or(args.name_pos).unwrap_or_else(|| {
                use std::time::{SystemTime, UNIX_EPOCH};
                let id = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
                    % 100000;
                format!("agent-{}", id)
            });

            if !args.json {
                println!("============================================================");
                println!(" Spinning Up VM Instance: {} (agentic readiness mode)", instance_name);
                println!("============================================================");
                println!("[*] Waiting for Linux guest kernel and SSH daemon readiness...");
            }

            let params = UpParams {
                name: instance_name.clone(),
                port: args.port,
                ram: args.ram,
                smp: args.smp,
                storage: args.storage,
                image: args.image,
                ephemeral: args.ephemeral,
            };

            let spin_res = orchestrator
                .spin(params, std::time::Duration::from_secs(args.timeout))
                .await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&spin_res)?);
            } else {
                let dur_secs = spin_res.boot_time_ms as f64 / 1000.0;
                println!("\n[✓] Virtual Machine is fully booted and ready in {:.2}s!", dur_secs);
                println!("------------------------------------------------------------");
                println!("  Name:          {}", spin_res.name);
                println!("  Port:          {}", spin_res.port);
                println!("  Storage:       {}", spin_res.storage.display());
                println!("  Ephemeral:     {}", spin_res.ephemeral);
                println!("  SSH Command:   {}", spin_res.ssh_command);
                println!("------------------------------------------------------------");
                println!("Execute commands inside the VM:");
                println!("  vmpod run {} \"uname -a\"", spin_res.name);
                println!("------------------------------------------------------------\n");
            }
        }

        Commands::Run(args) => {
            let command_str = args.command.join(" ");
            let run_res = orchestrator
                .run_command(
                    &args.name,
                    &command_str,
                    args.workdir.as_deref(),
                    &args.envs,
                    std::time::Duration::from_secs(args.timeout),
                )
                .await?;

            if args.json {
                println!("{}", serde_json::to_string_pretty(&run_res)?);
            } else {
                if !run_res.stdout.is_empty() {
                    print!("{}", run_res.stdout);
                }
                if !run_res.stderr.is_empty() {
                    eprint!("{}", run_res.stderr);
                }
                if run_res.timed_out {
                    eprintln!("Error: Command timed out after {}s", args.timeout);
                }
                if run_res.exit_code != 0 {
                    std::process::exit(run_res.exit_code);
                }
            }
        }

        Commands::Up(args) => {
            let base_name = args
                .name
                .or(args.name_pos)
                .unwrap_or_else(|| "vmpod-box".to_string());

            let count = args.count.max(1);
            let mut results = Vec::new();

            for i in 1..=count {
                let instance_name = if count > 1 {
                    format!("{}-{}", base_name, i)
                } else {
                    base_name.clone()
                };

                if !args.json {
                    println!("============================================================");
                    println!(" Launching VM Instance: {}", instance_name);
                    println!("============================================================");
                }

                // For batch provisioning, if a custom port was provided, increment it for subsequent instances
                let instance_port = args.port.map(|p| p + (i - 1) as u16);

                let instance_storage = args.storage.as_ref().map(|s| {
                    if count > 1 {
                        s.join(format!("instance-{}", i))
                    } else {
                        s.clone()
                    }
                });

                let params = UpParams {
                    name: instance_name,
                    port: instance_port,
                    ram: args.ram.clone(),
                    smp: args.smp,
                    storage: instance_storage,
                    image: args.image.clone(),
                    ephemeral: args.ephemeral,
                };

                let res = orchestrator.up(params).await?;

                if !args.json {
                    println!("\n[✓] Virtual Machine successfully started!");
                    println!("------------------------------------------------------------");
                    println!("  Name:          {}", res.name);
                    println!("  Container ID:  {} ({})", res.short_id, res.container_id);
                    println!("  Status:        {}", res.status);
                    println!("  Storage Path:  {}", res.storage_path.display());
                    println!("  SSH Command:   {}", res.ssh_command);
                    println!("------------------------------------------------------------");
                    println!("Connect via SSH:");
                    println!("  {}", res.ssh_command);
                    println!("  (Default credentials: user 'root', password 'root')");
                    println!(
                        "  (Or using private key: ssh -i {}/id_ed25519 root@localhost -p {})\n",
                        res.storage_path.display(),
                        res.port
                    );
                }

                results.push(res);
            }

            if args.json {
                if results.len() == 1 {
                    println!("{}", serde_json::to_string_pretty(&results[0])?);
                } else {
                    println!("{}", serde_json::to_string_pretty(&results)?);
                }
            }
        }

        Commands::Down(args) => {
            if args.all {
                let removed = orchestrator.down_all(args.purge).await?;
                if args.json {
                    println!("{}", serde_json::to_string_pretty(&removed)?);
                } else {
                    println!("============================================================");
                    println!(" Tearing Down ALL vmpod Instances");
                    println!("============================================================");
                    if removed.is_empty() {
                        println!("No active or stopped vmpod instances found on host.");
                    } else {
                        for res in &removed {
                            println!("[✓] Container '{}' stopped (purged storage: {}).", res.name, res.purged);
                        }
                        println!("------------------------------------------------------------");
                        println!("[✓] Teardown complete. {} instance(s) removed.", removed.len());
                    }
                }
            } else {
                let mut targets = args.names;
                if let Some(n) = args.name {
                    if !targets.contains(&n) {
                        targets.push(n);
                    }
                }

                if targets.is_empty() {
                    let active_instances = orchestrator.list_instances().await?;
                    if active_instances.is_empty() {
                        if args.json {
                            println!("[]");
                        } else {
                            println!("No active or stopped vmpod instances found on host.");
                        }
                        return Ok(());
                    } else if active_instances.len() == 1 {
                        let sole_name = active_instances[0].name.clone();
                        if !args.json {
                            println!("[*] No target specified; defaulting to only instance '{}'", sole_name);
                        }
                        targets.push(sole_name);
                    } else {
                        let names: Vec<&str> = active_instances.iter().map(|i| i.name.as_str()).collect();
                        println!("Multiple vmpod instances found: [{}]", names.join(", "));
                        println!("Please specify which instance to tear down: 'vmpod down <NAME>' or use 'vmpod down --all'.");
                        return Ok(());
                    }
                }

                let mut results = Vec::new();
                for target in targets {
                    if !args.json {
                        println!("============================================================");
                        println!(" Tearing Down VM Instance: {}", target);
                        println!("============================================================");
                        println!("[*] Gracefully stopping container '{}' (5-second timeout for QEMU disk flush)...", target);
                    }
                    let res = orchestrator.down(&target, args.purge).await?;
                    if !args.json {
                        println!("[✓] Container '{}' stopped and removed (purged storage: {}).", target, res.purged);
                    }
                    results.push(res);
                }

                if args.json {
                    if results.len() == 1 {
                        println!("{}", serde_json::to_string_pretty(&results[0])?);
                    } else {
                        println!("{}", serde_json::to_string_pretty(&results)?);
                    }
                }
            }
        }

        Commands::Status(args) => {
            let target_name = args.name.or(args.name_pos);

            match target_name {
                Some(name) => {
                    let res = orchestrator.status(&name).await?;

                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&res)?);
                    } else {
                        println!("============================================================");
                        println!(" Instance Status: {}", res.name);
                        println!("============================================================");
                        println!("  Container ID:  {} ({})", res.short_id, res.container_id);
                        println!(
                            "  Status:        {}",
                            if res.running {
                                format!("● {} (active)", res.status)
                            } else {
                                format!("○ {} (inactive)", res.status)
                            }
                        );
                        println!("  Uptime:        {}", res.uptime);
                        println!("  Image:         {}", res.image);

                        if let (Some(r), Some(s)) = (res.ram, res.smp) {
                            println!("  Resources:     {} vCPUs, {} RAM", s, r);
                        }

                        if let Some(bind) = res.storage_bind {
                            println!("  Storage Bind:  {}", bind);
                        }

                        if !res.port_mappings.is_empty() {
                            println!("  Port Mappings:");
                            for port_map in res.port_mappings {
                                println!("    - {}", port_map);
                            }
                        } else {
                            println!("  Port Mappings: None");
                        }

                        if let Some(ssh) = res.ssh_command {
                            if res.running {
                                println!("  SSH Command:   {}", ssh);
                            }
                        }
                        println!("------------------------------------------------------------");
                    }
                }
                None => {
                    let instances = orchestrator.list_instances().await?;
                    if args.json {
                        println!("{}", serde_json::to_string_pretty(&instances)?);
                    } else {
                        print_instances_table(&instances)?;
                    }
                }
            }
        }

        Commands::List(args) => {
            let instances = orchestrator.list_instances().await?;
            if args.json {
                println!("{}", serde_json::to_string_pretty(&instances)?);
            } else {
                print_instances_table(&instances)?;
            }
        }
    }

    Ok(())
}

fn print_instances_table(instances: &[orchestrator::InstanceSummary]) -> Result<()> {
    println!("=========================================================================================================");
    println!(" MANAGED VMPOD INSTANCES");
    println!("=========================================================================================================");

    if instances.is_empty() {
        println!("  No active or stopped vmpod instances found.");
        println!("  Run 'vmpod up' or 'vmpod spin' to launch a virtual machine.");
        println!("---------------------------------------------------------------------------------------------------------");
        return Ok(());
    }

    println!(
        " {:<16} {:<12} {:<14} {:<8} {:<10} {:<14} {:<24}",
        "NAME", "STATUS", "PORT", "CPUS", "RAM", "UPTIME", "SSH COMMAND"
    );
    println!("---------------------------------------------------------------------------------------------------------");

    for inst in instances {
        let status_colored = if inst.running {
            format!("● running")
        } else {
            format!("○ stopped")
        };

        let port_str = inst
            .port
            .map(|p| p.to_string())
            .unwrap_or_else(|| "-".to_string());

        println!(
            " {:<16} {:<12} {:<14} {:<8} {:<10} {:<14} {:<24}",
            inst.name,
            status_colored,
            port_str,
            inst.smp,
            inst.ram,
            inst.uptime,
            inst.ssh_command
        );
    }
    println!("---------------------------------------------------------------------------------------------------------");
    println!("Use 'vmpod status <NAME>' for details, or 'vmpod down <NAME>' to stop.");

    Ok(())
}

fn print_snapshots_table(vm_name: &str, snapshots: &[orchestrator::SnapshotInfo]) {
    println!("=========================================================================================================");
    println!(" SNAPSHOTS FOR VM: {}", vm_name);
    println!("=========================================================================================================");

    if snapshots.is_empty() {
        println!("  No snapshots found for VM '{}'.", vm_name);
        println!("  Run 'vmpod snapshot create {} <TAG>' to create a checkpoint.", vm_name);
        println!("---------------------------------------------------------------------------------------------------------");
        return;
    }

    println!(" {:<8} {:<24} {:<24} {:<16}", "ID", "TAG", "DATE", "SIZE");
    println!("---------------------------------------------------------------------------------------------------------");
    for s in snapshots {
        println!(" {:<8} {:<24} {:<24} {:<16}", s.id, s.tag, s.date, s.vm_size);
    }
    println!("---------------------------------------------------------------------------------------------------------");
    println!("Use 'vmpod rollback {} <TAG>' to revert to a snapshot.", vm_name);
}
