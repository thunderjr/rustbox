use clap::{Parser, Subcommand, ValueEnum};

pub mod commands;
pub mod output;

#[derive(Parser)]
#[command(name = "sandbox", about = "Rustbox CLI - manage sandboxes")]
pub struct Cli {
    /// Daemon URL
    #[arg(long, default_value = "http://localhost:8080", global = true)]
    pub url: String,

    /// Output format
    #[arg(long, default_value = "table", global = true)]
    pub output: OutputFormat,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Clone, ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Create a new sandbox
    Create {
        /// Runtime environment
        #[arg(long)]
        runtime: String,
        /// Timeout in seconds
        #[arg(long, default_value = "300")]
        timeout: u64,
        /// Restore from snapshot
        #[arg(long)]
        snapshot: Option<String>,
    },
    /// List sandboxes
    List {
        /// Show all (including stopped)
        #[arg(long)]
        all: bool,
    },
    /// Stop a sandbox
    Stop {
        /// Sandbox ID
        id: String,
    },
    /// Execute a command in a sandbox
    Exec {
        /// Sandbox ID
        id: String,
        /// Run as root
        #[arg(long)]
        sudo: bool,
        /// Working directory
        #[arg(long)]
        workdir: Option<String>,
        /// Environment variables (KEY=VALUE)
        #[arg(long, value_name = "KEY=VALUE")]
        env: Vec<String>,
        /// Command and arguments
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Copy files between host and sandbox
    Copy {
        /// Source path (host:path or sandbox_id:path)
        src: String,
        /// Destination path (host:path or sandbox_id:path)
        dst: String,
    },
    /// Create and run a command in a temporary sandbox
    Run {
        /// Runtime environment
        #[arg(long)]
        runtime: String,
        /// Remove sandbox after command exits
        #[arg(long)]
        rm: bool,
        /// Command and arguments
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Connect to a sandbox (interactive shell)
    Connect {
        /// Sandbox ID
        id: String,
    },
    /// Snapshot management
    Snapshot {
        #[command(subcommand)]
        command: SnapshotCommands,
    },
}

#[derive(Subcommand)]
pub enum SnapshotCommands {
    /// Create a snapshot
    Create {
        /// Sandbox ID
        id: String,
    },
    /// List snapshots
    List,
    /// Get snapshot details
    Get {
        /// Snapshot ID
        id: String,
    },
    /// Delete a snapshot
    Delete {
        /// Snapshot ID
        id: String,
    },
}

/// Run the CLI (placeholder - actual execution in Phase 3 SDK integration)
pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Create {
            runtime,
            timeout,
            snapshot,
        } => {
            println!("Creating sandbox: runtime={runtime}, timeout={timeout}s");
            if let Some(snap) = snapshot {
                println!("  from snapshot: {snap}");
            }
        }
        Commands::List { all } => {
            println!("Listing sandboxes (all={all})");
        }
        Commands::Stop { id } => {
            println!("Stopping sandbox {id}");
        }
        Commands::Exec {
            id,
            sudo: _,
            workdir: _,
            env: _,
            cmd,
        } => {
            println!("Exec in {id}: {:?}", cmd);
        }
        Commands::Copy { src, dst } => {
            println!("Copy {src} -> {dst}");
        }
        Commands::Run {
            runtime,
            rm,
            cmd,
        } => {
            println!("Run in {runtime}: {:?} (rm={rm})", cmd);
        }
        Commands::Connect { id } => {
            println!("Connect to {id}");
        }
        Commands::Snapshot { command } => match command {
            SnapshotCommands::Create { id } => println!("Snapshot create {id}"),
            SnapshotCommands::List => println!("Snapshot list"),
            SnapshotCommands::Get { id } => println!("Snapshot get {id}"),
            SnapshotCommands::Delete { id } => println!("Snapshot delete {id}"),
        },
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parse_create_with_runtime() {
        let cli = Cli::try_parse_from(["sandbox", "create", "--runtime", "node24"]).unwrap();
        matches!(cli.command, Commands::Create { runtime, .. } if runtime == "node24");
    }

    #[test]
    fn parse_list() {
        let cli = Cli::try_parse_from(["sandbox", "list"]).unwrap();
        assert!(matches!(cli.command, Commands::List { all: false }));
    }

    #[test]
    fn parse_list_all() {
        let cli = Cli::try_parse_from(["sandbox", "list", "--all"]).unwrap();
        assert!(matches!(cli.command, Commands::List { all: true }));
    }

    #[test]
    fn parse_exec_with_trailing_args() {
        let cli =
            Cli::try_parse_from(["sandbox", "exec", "abc123", "--", "echo", "hello"]).unwrap();
        match cli.command {
            Commands::Exec { id, cmd, .. } => {
                assert_eq!(id, "abc123");
                assert_eq!(cmd, vec!["echo", "hello"]);
            }
            _ => panic!("expected Exec"),
        }
    }

    #[test]
    fn parse_copy() {
        let cli =
            Cli::try_parse_from(["sandbox", "copy", "host:/tmp/a", "abc123:/data/a"]).unwrap();
        match cli.command {
            Commands::Copy { src, dst } => {
                assert_eq!(src, "host:/tmp/a");
                assert_eq!(dst, "abc123:/data/a");
            }
            _ => panic!("expected Copy"),
        }
    }

    #[test]
    fn parse_run() {
        let cli = Cli::try_parse_from([
            "sandbox", "run", "--runtime", "node24", "--rm", "--", "node", "index.js",
        ])
        .unwrap();
        match cli.command {
            Commands::Run { runtime, rm, cmd } => {
                assert_eq!(runtime, "node24");
                assert!(rm);
                assert_eq!(cmd, vec!["node", "index.js"]);
            }
            _ => panic!("expected Run"),
        }
    }

    #[test]
    fn parse_snapshot_create() {
        let cli =
            Cli::try_parse_from(["sandbox", "snapshot", "create", "abc123"]).unwrap();
        match cli.command {
            Commands::Snapshot {
                command: SnapshotCommands::Create { id },
            } => {
                assert_eq!(id, "abc123");
            }
            _ => panic!("expected Snapshot Create"),
        }
    }

    #[test]
    fn parse_snapshot_list() {
        let cli = Cli::try_parse_from(["sandbox", "snapshot", "list"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Snapshot {
                command: SnapshotCommands::List
            }
        ));
    }

    #[test]
    fn parse_snapshot_delete() {
        let cli =
            Cli::try_parse_from(["sandbox", "snapshot", "delete", "snap-id"]).unwrap();
        match cli.command {
            Commands::Snapshot {
                command: SnapshotCommands::Delete { id },
            } => {
                assert_eq!(id, "snap-id");
            }
            _ => panic!("expected Snapshot Delete"),
        }
    }

    #[test]
    fn custom_url() {
        let cli =
            Cli::try_parse_from(["sandbox", "--url", "http://custom:9090", "list"]).unwrap();
        assert_eq!(cli.url, "http://custom:9090");
    }

    #[test]
    fn output_json() {
        let cli = Cli::try_parse_from(["sandbox", "--output", "json", "list"]).unwrap();
        assert!(matches!(cli.output, OutputFormat::Json));
    }

    #[test]
    fn missing_required_args() {
        assert!(Cli::try_parse_from(["sandbox", "create"]).is_err());
    }
}
