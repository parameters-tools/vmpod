mod orchestrator;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use orchestrator::{UpParams, VmOrchestrator};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "vmpod",
    author = "vmpod contributors",
    version = "1.0.0",
    about = "Fast, local-first CLI to spin up and orchestrate hardware-accelerated Linux VMs inside Docker using QEMU/KVM",
    long_about = "vmpod allows you to launch lightweight, hardware-accelerated virtual machines inside Docker containers \
                  with direct KVM pass-through, user-space networking, and automatic root credentials provisioning."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Spin up a new hardware-accelerated virtual machine container
    Up(UpArgs),
    /// Gracefully stop and remove a virtual machine container
    Down(DownArgs),
    /// Inspect the status and health of a virtual machine container
    Status(StatusArgs),
}

#[derive(Args, Debug)]
struct UpArgs {
    /// Unique identifier for the VM container
    #[arg(long, default_value = "vmpod-box", value_name = "NAME")]
    name: String,

    /// Host port mapped to the VM's SSH port 22
    #[arg(long, default_value_t = 2222, value_name = "PORT")]
    port: u16,

    /// Guest RAM allocation (passed as environment variable to runner)
    #[arg(long, default_value = "2G", value_name = "RAM")]
    ram: String,

    /// Guest vCPU core count (passed as environment variable to runner)
    #[arg(long, default_value_t = 2, value_name = "CPUS")]
    smp: u32,

    /// Host directory where virtual disk images live
    #[arg(long, default_value = "./vmpod-data", value_name = "PATH")]
    storage: PathBuf,

    /// Docker image running QEMU
    #[arg(long, default_value = "vmpod/qemu-runner:latest", value_name = "IMAGE")]
    image: String,
}

#[derive(Args, Debug)]
struct DownArgs {
    /// Name of the VM instance to tear down
    #[arg(long, default_value = "vmpod-box", value_name = "NAME")]
    name: String,
}

#[derive(Args, Debug)]
struct StatusArgs {
    /// Name of the VM instance to inspect
    #[arg(long, default_value = "vmpod-box", value_name = "NAME")]
    name: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let orchestrator = VmOrchestrator::new()?;

    match cli.command {
        Commands::Up(args) => {
            println!("============================================================");
            println!(" Launching VM Instance: {}", args.name);
            println!("============================================================");

            let params = UpParams {
                name: args.name,
                port: args.port,
                ram: args.ram,
                smp: args.smp,
                storage: args.storage,
                image: args.image,
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

        Commands::Down(args) => {
            println!("============================================================");
            println!(" Tearing Down VM Instance: {}", args.name);
            println!("============================================================");

            orchestrator.down(&args.name).await?;
        }

        Commands::Status(args) => {
            let res = orchestrator.status(&args.name).await?;

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

    Ok(())
}
