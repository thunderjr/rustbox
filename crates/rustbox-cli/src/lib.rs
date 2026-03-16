use clap::{Parser, Subcommand, ValueEnum};
use rustbox_core::command::CommandStatus;
use rustbox_core::sandbox::Runtime;
use rustbox_sdk::client::{CommandInfo, SandboxInfo, SnapshotInfo};
use rustbox_sdk::RustboxClient;
use std::collections::HashMap;
use std::process;

pub mod commands;
pub mod output;

use output::{format_json, format_table};

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

fn parse_runtime(s: &str) -> anyhow::Result<Runtime> {
    match s {
        "node24" => Ok(Runtime::Node24),
        "node22" => Ok(Runtime::Node22),
        "python313" => Ok(Runtime::Python313),
        _ => anyhow::bail!(
            "unknown runtime '{s}'. Valid options: node24, node22, python313"
        ),
    }
}

fn parse_env_vars(env: &[String]) -> anyhow::Result<HashMap<String, String>> {
    let mut map = HashMap::new();
    for entry in env {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| anyhow::anyhow!("invalid env format '{entry}', expected KEY=VALUE"))?;
        map.insert(key.to_string(), value.to_string());
    }
    Ok(map)
}

/// Parse a copy path like "sandbox_id:/remote/path" or "host:/local/path".
/// Returns (is_host, id_or_host, path).
fn parse_copy_path(s: &str) -> anyhow::Result<(bool, String, String)> {
    let (id, path) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("invalid copy path '{s}', expected ID:PATH or host:PATH"))?;
    let is_host = id == "host";
    Ok((is_host, id.to_string(), path.to_string()))
}

fn format_sandbox_table(sandboxes: &[SandboxInfo]) -> String {
    let headers = &["ID", "STATUS", "RUNTIME", "CREATED"];
    let rows: Vec<Vec<String>> = sandboxes
        .iter()
        .map(|s| {
            vec![
                s.id.clone(),
                format!("{:?}", s.status).to_lowercase(),
                format!("{:?}", s.runtime).to_lowercase(),
                s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
            ]
        })
        .collect();
    format_table(headers, &rows)
}

fn format_snapshot_table(snapshots: &[SnapshotInfo]) -> String {
    let headers = &["ID", "SANDBOX", "CREATED", "SIZE", "DESCRIPTION"];
    let rows: Vec<Vec<String>> = snapshots
        .iter()
        .map(|s| {
            vec![
                s.id.clone(),
                s.sandbox_id.clone(),
                s.created_at.format("%Y-%m-%d %H:%M:%S").to_string(),
                format!("{} B", s.size_bytes),
                s.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    format_table(headers, &rows)
}

fn print_command_output(info: &CommandInfo) {
    for entry in &info.output {
        match entry.stream.as_str() {
            "stdout" => {
                if let Some(data) = &entry.data {
                    print!("{}", String::from_utf8_lossy(data));
                }
            }
            "stderr" => {
                if let Some(data) = &entry.data {
                    eprint!("{}", String::from_utf8_lossy(data));
                }
            }
            _ => {}
        }
    }
}

pub async fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let client = RustboxClient::new(&cli.url);

    match cli.command {
        Commands::Create {
            runtime,
            timeout,
            snapshot: _,
        } => {
            let rt = parse_runtime(&runtime)?;
            let sandbox = client.create_sandbox(rt, timeout).await?;
            match cli.output {
                OutputFormat::Json => println!("{}", format_json(&sandbox)),
                OutputFormat::Table => {
                    println!("{}", sandbox.id);
                }
            }
        }
        Commands::List { all: _ } => {
            let sandboxes = client.list_sandboxes().await?;
            match cli.output {
                OutputFormat::Json => println!("{}", format_json(&sandboxes)),
                OutputFormat::Table => print!("{}", format_sandbox_table(&sandboxes)),
            }
        }
        Commands::Stop { id } => {
            client.delete_sandbox(&id).await?;
            eprintln!("{id}");
        }
        Commands::Exec {
            id,
            sudo,
            workdir,
            env,
            cmd,
        } => {
            let env_map = parse_env_vars(&env)?;
            let env_opt = if env_map.is_empty() {
                None
            } else {
                Some(&env_map)
            };
            let (first, rest) = cmd
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("command required"))?;
            let cmd_id = client
                .exec_full(&id, first, rest, workdir.as_deref(), env_opt, sudo, false)
                .await?;

            // Poll for completion
            loop {
                let info = client.get_command(&id, &cmd_id).await?;
                match &info.status {
                    CommandStatus::Running => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    CommandStatus::Completed(code) => {
                        print_command_output(&info);
                        if *code != 0 {
                            process::exit(*code);
                        }
                        break;
                    }
                    CommandStatus::Failed(msg) => {
                        print_command_output(&info);
                        anyhow::bail!("command failed: {msg}");
                    }
                    CommandStatus::Killed => {
                        print_command_output(&info);
                        anyhow::bail!("command was killed");
                    }
                }
            }
        }
        Commands::Copy { src, dst } => {
            let (src_is_host, src_id, src_path) = parse_copy_path(&src)?;
            let (dst_is_host, dst_id, dst_path) = parse_copy_path(&dst)?;

            if src_is_host && !dst_is_host {
                // Upload: host → sandbox
                let content = tokio::fs::read(&src_path).await?;
                client.upload_file(&dst_id, &dst_path, &content).await?;
            } else if !src_is_host && dst_is_host {
                // Download: sandbox → host
                let content = client.download_file(&src_id, &src_path).await?;
                tokio::fs::write(&dst_path, &content).await?;
            } else if src_is_host && dst_is_host {
                anyhow::bail!("both source and destination are host paths");
            } else {
                anyhow::bail!("sandbox-to-sandbox copy not supported; copy through host instead");
            }
        }
        Commands::Run { runtime, rm, cmd } => {
            let rt = parse_runtime(&runtime)?;
            let sandbox = client.create_sandbox(rt, 300).await?;
            let sandbox_id = sandbox.id.clone();

            let (first, rest) = cmd
                .split_first()
                .ok_or_else(|| anyhow::anyhow!("command required"))?;
            let cmd_id = client
                .exec_full(&sandbox_id, first, rest, None, None, false, false)
                .await?;

            let exit_code;
            loop {
                let info = client.get_command(&sandbox_id, &cmd_id).await?;
                match &info.status {
                    CommandStatus::Running => {
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                    }
                    CommandStatus::Completed(code) => {
                        print_command_output(&info);
                        exit_code = *code;
                        break;
                    }
                    CommandStatus::Failed(msg) => {
                        print_command_output(&info);
                        if rm {
                            let _ = client.delete_sandbox(&sandbox_id).await;
                        }
                        anyhow::bail!("command failed: {msg}");
                    }
                    CommandStatus::Killed => {
                        print_command_output(&info);
                        if rm {
                            let _ = client.delete_sandbox(&sandbox_id).await;
                        }
                        anyhow::bail!("command was killed");
                    }
                }
            }

            if rm {
                client.delete_sandbox(&sandbox_id).await?;
            }

            if exit_code != 0 {
                process::exit(exit_code);
            }
        }
        Commands::Connect { id } => {
            // Start an interactive shell
            let cmd_id = client
                .exec_full(&id, "/bin/bash", &[], None, None, false, false)
                .await?;
            eprintln!("connected to {id} (command {cmd_id})");
            eprintln!("note: interactive mode not yet supported, use 'sandbox exec' instead");
        }
        Commands::Snapshot { command } => match command {
            SnapshotCommands::Create { id } => {
                let snap = client.create_snapshot(&id, None).await?;
                match cli.output {
                    OutputFormat::Json => println!("{}", format_json(&snap)),
                    OutputFormat::Table => println!("{}", snap.id),
                }
            }
            SnapshotCommands::List => {
                let snapshots = client.list_snapshots().await?;
                match cli.output {
                    OutputFormat::Json => println!("{}", format_json(&snapshots)),
                    OutputFormat::Table => print!("{}", format_snapshot_table(&snapshots)),
                }
            }
            SnapshotCommands::Get { id } => {
                let snap = client.get_snapshot(&id).await?;
                match cli.output {
                    OutputFormat::Json => println!("{}", format_json(&snap)),
                    OutputFormat::Table => {
                        println!("ID:          {}", snap.id);
                        println!("Sandbox:     {}", snap.sandbox_id);
                        println!("Created:     {}", snap.created_at);
                        println!("Size:        {} bytes", snap.size_bytes);
                        if let Some(desc) = &snap.description {
                            println!("Description: {desc}");
                        }
                    }
                }
            }
            SnapshotCommands::Delete { id } => {
                client.delete_snapshot(&id).await?;
                eprintln!("{id}");
            }
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
        match cli.command {
            Commands::Create { runtime, .. } => assert_eq!(runtime, "node24"),
            _ => panic!("expected Create"),
        }
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
        let cli = Cli::try_parse_from(["sandbox", "snapshot", "create", "abc123"]).unwrap();
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
        let cli = Cli::try_parse_from(["sandbox", "snapshot", "delete", "snap-id"]).unwrap();
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

    #[test]
    fn parse_runtime_valid() {
        assert!(parse_runtime("node24").is_ok());
        assert!(parse_runtime("node22").is_ok());
        assert!(parse_runtime("python313").is_ok());
    }

    #[test]
    fn parse_runtime_invalid() {
        assert!(parse_runtime("ruby33").is_err());
    }

    #[test]
    fn parse_env_vars_valid() {
        let vars = vec!["KEY=value".to_string(), "FOO=bar=baz".to_string()];
        let map = parse_env_vars(&vars).unwrap();
        assert_eq!(map.get("KEY").unwrap(), "value");
        assert_eq!(map.get("FOO").unwrap(), "bar=baz");
    }

    #[test]
    fn parse_env_vars_invalid() {
        let vars = vec!["NOEQUALS".to_string()];
        assert!(parse_env_vars(&vars).is_err());
    }

    #[test]
    fn parse_copy_path_host() {
        let (is_host, id, path) = parse_copy_path("host:/tmp/file.txt").unwrap();
        assert!(is_host);
        assert_eq!(id, "host");
        assert_eq!(path, "/tmp/file.txt");
    }

    #[test]
    fn parse_copy_path_sandbox() {
        let (is_host, id, path) = parse_copy_path("abc123:/data/file.txt").unwrap();
        assert!(!is_host);
        assert_eq!(id, "abc123");
        assert_eq!(path, "/data/file.txt");
    }

    #[test]
    fn parse_copy_path_invalid() {
        assert!(parse_copy_path("nocolon").is_err());
    }
}
