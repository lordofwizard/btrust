use clap::Parser;
use colored::*;
use std::fs::{self};
use std::io::{self, BufRead};
use std::path::{Path, PathBuf};
use std::process::{Command, exit};
use nix::unistd::Uid;

#[derive(Parser)]
#[command(name = "BtRust")]
#[command(about = "A tool to recover files from Btrfs file systems")]
#[command(long_about = "BtRust is designed to facilitate file recovery from Btrfs file systems. \
    It allows users to list recoverable files, perform dry-run recovery, and restore files using regex patterns or explicit paths.")]
struct Args {
    /// Specify the Btrfs device to perform recovery on
    #[arg(short = 'd', long = "device", required = true)]
    device: String,

    /// Specify the output directory where recovered files will be stored (use . for current directory)
    #[arg(short = 'o', long = "output", required = true)]
    output: PathBuf,

    /// Specify paths or regex patterns for files to recover (use ".*" to recover all files)
    #[arg(short = 'p', long = "path")]
    paths: Vec<String>,

    /// Set depth level of recovery (0-2, default: 0)
    #[arg(short = 'l', long = "level", default_value = "0")]
    #[arg(value_parser = clap::value_parser!(u8).range(0..=2))]
    level: u8,

    /// List all recoverable files at specified depth without recovering
    #[arg(short = 'L', long = "list")]
    #[arg(value_parser = clap::value_parser!(u8).range(0..=2))]
    list: Option<u8>,

    /// Perform a dry run without actual recovery
    #[arg(short = 'n', long = "dry-run")]
    dry_run: bool,
}

struct RecoveryContext {
    roots_file: String,
    tmp_file: String,
    regex: String,
}

impl RecoveryContext {
    fn new() -> Self {
        RecoveryContext {
            roots_file: String::from("/tmp/btrfsroots.tmp"),
            tmp_file: String::from("/tmp/undeleter.tmp"),
            regex: String::new(),
        }
    }
}

fn check_root() -> io::Result<()> {
    if !Uid::effective().is_root() {
        eprintln!("{}", "Error: This program must be run with sudo (or as root)".red());
        exit(1);
    }
    Ok(())
}

fn check_device(device: &str) -> io::Result<()> {
    if !Path::new(device).exists() {
        eprintln!("{} {} {}", 
            "Error: Device".red(),
            device.blue(),
            "doesn't exist!".yellow());
        exit(1);
    }
    Ok(())
}

fn check_output_dir(dir: &Path) -> io::Result<()> {
    if !dir.exists() || !dir.is_dir() {
        eprintln!("{} {} {}", 
            "Error: Directory".red(),
            dir.to_string_lossy().blue(),
            "doesn't exist!".yellow());
        exit(1);
    }
    Ok(())
}

fn check_mount(device: &str) -> io::Result<()> {
    let mtab = fs::read_to_string("/etc/mtab")?;
    if mtab.lines().any(|line| line.contains(device)) {
        eprintln!("{} {} {}", 
            "Error:".red(),
            device.blue(),
            "is mounted! Please unmount first.".yellow());
        exit(1);
    }
    Ok(())
}

fn debug_command_output(cmd: &mut Command) -> io::Result<()> {
    let output = cmd.output()?;
    if !output.status.success() {
        println!("Command failed with status: {:?}", output.status);
        println!("stderr: {}", String::from_utf8_lossy(&output.stderr));
        println!("stdout: {}", String::from_utf8_lossy(&output.stdout));
    }
    Ok(())
}

fn list_files(device: &str, depth: u8, ctx: &RecoveryContext) -> io::Result<()> {
    println!("Listing recoverable files at depth {}...", depth);

    if depth == 0 {
        let output = Command::new("btrfs")
            .args(["restore", "-Divv", "--path-regex", "^/.*$", device, "/"])
            .output()?;
        process_and_display_files(&output.stderr)?;
    } else {
        let roots = generate_roots(device, depth, ctx)?;
        for root in roots {
            let output = Command::new("btrfs")
                .args(["restore", "-t", &root, "-Divv", "--path-regex", "^/.*$", device, "/"])
                .output()?;
            process_and_display_files(&output.stderr)?;
        }
    }
    Ok(())
}

fn process_and_display_files(output: &[u8]) -> io::Result<()> {
    let output_str = String::from_utf8_lossy(output);
    let mut files = Vec::new();

    for line in output_str.lines() {
        if line.contains("Restoring") {
            if let Some(path) = line.split_whitespace().nth(1) {
                files.push(path.to_string());
            }
        }
    }

    files.sort();
    files.dedup();
    for file in files {
        println!("{}", file);
    }
    Ok(())
}

fn generate_roots(device: &str, depth: u8, ctx: &RecoveryContext) -> io::Result<Vec<String>> {
    let args = if depth == 2 {
        println!("{}", "Note: Level 2 search may take longer and produce more results".yellow());
        vec!["-a"]
    } else {
        vec![]
    };

    let output = Command::new("btrfs-find-root")
        .args(&args)
        .arg(device)
        .output()?;

    let output_str = String::from_utf8_lossy(&output.stdout);
    let mut roots = Vec::new();

    for line in output_str.lines() {
        if line.contains("Well block") {
            if let Some(root) = line
                .split("Well block")
                .nth(1)
                .and_then(|s| s.split_whitespace().next())
                .and_then(|s| s.trim_matches(|c: char| !c.is_digit(10)).parse::<u64>().ok())
            {
                roots.push(root.to_string());
            }
        }
    }

    if roots.is_empty() {
        println!("{}", "Warning: No valid roots found".yellow());
    } else {
        println!("Found {} roots", roots.len());
    }

    roots.sort_by(|a, b| b.parse::<u64>().unwrap_or(0).cmp(&a.parse::<u64>().unwrap_or(0)));
    fs::write(&ctx.roots_file, roots.join("\n"))?;
    
    Ok(roots)
}

fn build_regex(paths: &[String]) -> String {
    if paths.len() == 1 && paths[0] == ".*" {
        return "^/.*$".to_string();
    }

    let patterns: Vec<String> = paths.iter()
        .map(|path| {
            let clean_path = path.trim_start_matches('/');
            format!("^/{}$", clean_path)
        })
        .collect();

    patterns.join("|")
}

fn perform_recovery(device: &str, output_dir: &Path, paths: &[String], depth: u8, dry_run: bool, ctx: &RecoveryContext) -> io::Result<()> {
    let regex = build_regex(paths);
    
    if dry_run {
        println!("Performing dry run at depth {}...", depth);
        perform_dry_run(device, &regex, depth, ctx)?;
        return Ok(());
    }

    println!("Starting recovery at depth {}...", depth);
    
    if depth == 0 {
        let mut cmd = Command::new("btrfs");
        cmd.args(["restore", "-ivv", "--path-regex", &regex, device, output_dir.to_str().unwrap()]);
        debug_command_output(&mut cmd)?;
    } else {
        let roots = generate_roots(device, depth, ctx)?;
        if roots.is_empty() {
            eprintln!("{}", "Error: No valid roots found for recovery".red());
            return Ok(());
        }

        for root in roots {
            println!("Processing root: {}", root);
            let mut cmd = Command::new("btrfs");
            cmd.args([
                "restore", "-x", "-m", "-t", &root, "-ivv",
                "--path-regex", &regex, device,
                output_dir.to_str().unwrap()
            ]);
            
            if let Err(e) = debug_command_output(&mut cmd) {
                println!("{}", format!("Warning: Recovery from root {} failed: {}", root, e).yellow());
                continue;
            }
            
            remove_empty_files(output_dir)?;
        }
    }

    remove_empty_files(output_dir)?;
    print_recovery_summary(output_dir)?;
    
    Ok(())
}

fn perform_dry_run(device: &str, regex: &str, depth: u8, ctx: &RecoveryContext) -> io::Result<()> {
    println!("Files that would be recovered:");

    if depth == 0 {
        let output = Command::new("btrfs")
            .args(["restore", "-Divv", "--path-regex", regex, device, "/"])
            .output()?;
        process_and_display_files(&output.stderr)?;
    } else {
        let roots = generate_roots(device, depth, ctx)?;
        for root in roots {
            let output = Command::new("btrfs")
                .args(["restore", "-t", &root, "-Divv", "--path-regex", regex, device, "/"])
                .output()?;
            process_and_display_files(&output.stderr)?;
        }
    }
    
    Ok(())
}

fn remove_empty_files(dir: &Path) -> io::Result<()> {
    Command::new("find")
        .args([
            dir.to_str().unwrap(),
            "-empty",
            "-type",
            "f",
            "-delete"
        ])
        .status()?;
    Ok(())
}

fn print_recovery_summary(dir: &Path) -> io::Result<()> {
    let output = Command::new("find")
        .args([
            dir.to_str().unwrap(),
            "!",
            "-empty",
            "-type",
            "f"
        ])
        .output()?;

    let files: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(String::from)
        .collect();

    println!("\n{} {} {}", 
        "Recovery completed:".green(),
        files.len().to_string().blue(),
        "files recovered");

    if !files.is_empty() {
        println!("\nSample of recovered files:");
        for file in files.iter().take(5) {
            println!("{}", file);
        }
    }

    Ok(())
}

fn main() -> io::Result<()> {
    let args = Args::parse();
    let ctx = RecoveryContext::new();

    check_root()?;
    check_device(&args.device)?;
    check_output_dir(&args.output)?;
    check_mount(&args.device)?;

    if let Some(list_depth) = args.list {
        list_files(&args.device, list_depth, &ctx)?;
    } else {
        if args.paths.is_empty() {
            eprintln!("{}", "Error: At least one path must be specified with -p/--path".red());
            exit(1);
        }
        perform_recovery(
            &args.device,
            &args.output,
            &args.paths,
            args.level,
            args.dry_run,
            &ctx
        )?;
    }

    // Cleanup
    if Path::new(&ctx.roots_file).exists() {
        fs::remove_file(&ctx.roots_file)?;
    }
    if Path::new(&ctx.tmp_file).exists() {
        fs::remove_file(&ctx.tmp_file)?;
    }

    Ok(())
}
