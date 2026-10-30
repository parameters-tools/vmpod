mod orchestrator;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use orchestrator::{UpParams, VmOrchestrator};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vmpod",
    author = "vmpod contributors",
    version = "1.1.0",
    about = "Fast, local-first CLI to spin up and orchestrate hardware-accelerated Linux VMs inside Docker using QEMU/KVM",
    long_about = "vmpod allows you to launch lightweight, hardware-accelerated virtual machines inside Docker containers \
                  with direct KVM pass-through, user-space networking, automatic root credentials provisioning, \
                  and multi-instance orchestration."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Spin up hardware-accelerated virtual machine container(s)
    Up(UpArgs),
    /// Gracefully stop and remove virtual machine container(s)
    Down(DownArgs),
    /// Inspect the status and health of a virtual machine container
    Status(StatusArgs),
    /// List all managed virtual machine instances
    #[command(alias = "ls", alias = "ps")]
    List,
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
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Name of the VM instance to inspect (positional)
    #[arg(value_name = "NAME")]
    name_pos: Option<String>,

    /// Name of the VM instance to inspect (flag)
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let orchestrator = VmOrchestrator::new()?;

    match cli.command {
        Commands::Up(args) => {
            let base_name = args
                .name
                .or(args.name_pos)
                .unwrap_or_else(|| "vmpod-box".to_string());

            let count = args.count.max(1);

            for i in 1..=count {
                let instance_name = if count > 1 {
                    format!("{}-{}", base_name, i)
                } else {
                    base_name.clone()
                };

                println!("============================================================");
                println!(" Launching VM Instance: {}", instance_name);
                println!("============================================================");

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
                };

                let res = orchestrator.up(params).await?;

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
        }

        Commands::Down(args) => {
            if args.all {
                println!("============================================================");
                println!(" Tearing Down ALL vmpod Instances");
                println!("============================================================");
                let removed = orchestrator.down_all().await?;
                println!("------------------------------------------------------------");
                println!("[✓] Teardown complete. {} instance(s) removed.", removed.len());
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
                        println!("No active or stopped vmpod instances found on host.");
                        return Ok(());
                    } else if active_instances.len() == 1 {
                        let sole_name = active_instances[0].name.clone();
                        println!("[*] No target specified; defaulting to only instance '{}'", sole_name);
                        targets.push(sole_name);
                    } else {
                        let names: Vec<&str> = active_instances.iter().map(|i| i.name.as_str()).collect();
                        println!("Multiple vmpod instances found: [{}]", names.join(", "));
                        println!("Please specify which instance to tear down: 'vmpod down <NAME>' or use 'vmpod down --all'.");
                        return Ok(());
                    }
                }

                for target in targets {
                    println!("============================================================");
                    println!(" Tearing Down VM Instance: {}", target);
                    println!("============================================================");
                    orchestrator.down(&target).await?;
                }
            }
        }

        Commands::Status(args) => {
            let target_name = args.name.or(args.name_pos);

            match target_name {
                Some(name) => {
                    let res = orchestrator.status(&name).await?;

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
                None => {
                    // When no name is specified, show list of all instances
                    print_instances_table(&orchestrator).await?;
                }
            }
        }

        Commands::List => {
            print_instances_table(&orchestrator).await?;
        }
    }

    Ok(())
}

async fn print_instances_table(orchestrator: &VmOrchestrator) -> Result<()> {
    let instances = orchestrator.list_instances().await?;

    println!("=========================================================================================================");
    println!(" MANAGED VMPOD INSTANCES");
    println!("=========================================================================================================");

    if instances.is_empty() {
        println!("  No active or stopped vmpod instances found.");
        println!("  Run 'vmpod up' to launch a virtual machine.");
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
