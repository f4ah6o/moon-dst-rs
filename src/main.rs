// SPDX-License-Identifier: MIT
//! moon-dst: MoonBit dependency updater CLI

mod telemetry;
mod telemetry_span;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use opentelemetry::KeyValue;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use telemetry::{mask_cli_args, RootAttrs, Telemetry};
use walkdir::WalkDir;

// =============================================================================
// CLI Definitions
// =============================================================================

#[derive(Parser)]
#[command(name = "moon-dst")]
#[command(about = "MoonBit dependency updater CLI (moon dust)")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Scan for moon.mod.json files and list dependencies
    Scan {
        #[command(flatten)]
        common: CommonOptions,

        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Apply dependency updates (moon update + moon add)
    Apply {
        #[command(flatten)]
        common: CommonOptions,

        /// Skip initial moon update
        #[arg(long)]
        skip_update: bool,

        /// Number of times to repeat moon add (default: 1)
        #[arg(long, default_value = "1")]
        repeat: u32,

        /// Only update specific packages (can be specified multiple times)
        #[arg(long = "package", short = 'p')]
        packages: Vec<String>,

        /// Stop on first failure
        #[arg(long)]
        fail_fast: bool,

        /// Skip adding justfile to repos
        #[arg(long)]
        no_justfile: bool,

        /// Justfile handling mode
        #[arg(long, value_enum, default_value = "create")]
        justfile_mode: JustfileMode,
    },

    /// Add justfile to repos
    Just {
        #[command(flatten)]
        common: CommonOptions,

        /// Justfile handling mode
        #[arg(long, value_enum, default_value = "create")]
        mode: JustfileMode,
    },
}

#[derive(Parser)]
struct CommonOptions {
    /// Root directory to search from
    #[arg(long, default_value = ".")]
    root: PathBuf,

    /// Paths or directory names to ignore (can be specified multiple times)
    #[arg(long = "ignore", short = 'i')]
    ignores: Vec<String>,

    /// Disable default ignore rules
    #[arg(long)]
    no_default_ignore: bool,

    /// Number of parallel jobs (default: CPU cores / 2)
    #[arg(long, short = 'j')]
    jobs: Option<usize>,

    /// Show commands without executing
    #[arg(long)]
    dry_run: bool,

    /// Enable verbose output
    #[arg(long, short = 'v')]
    verbose: bool,
}

#[derive(Clone, Copy, ValueEnum, Default)]
enum JustfileMode {
    /// Skip if justfile exists
    Skip,
    /// Create only if missing
    #[default]
    Create,
    /// Merge with existing (not implemented)
    Merge,
}

// =============================================================================
// Data Structures
// =============================================================================

/// moon.mod.json structure
#[derive(Deserialize, Debug)]
struct MoonMod {
    #[serde(default)]
    deps: HashMap<String, serde_json::Value>,
}

/// Discovered moon.mod.json info
#[derive(Debug, Clone)]
struct MoonModInfo {
    path: PathBuf,
    deps: Vec<String>,
}

/// Repository information
#[derive(Debug, Clone)]
struct RepoInfo {
    root: PathBuf,
    moon_mods: Vec<MoonModInfo>,
}

/// JSON output structure for scan
#[derive(Serialize)]
struct ScanOutput {
    repos: Vec<RepoOutput>,
}

#[derive(Serialize)]
struct RepoOutput {
    repo_root: String,
    moon_mods: Vec<MoonModOutput>,
}

#[derive(Serialize)]
struct MoonModOutput {
    path: String,
    deps: Vec<String>,
}

/// Execution result for a repo
#[derive(Debug)]
struct RepoResult {
    repo_root: PathBuf,
    success: bool,
    updated_packages: Vec<String>,
    failed_packages: Vec<(String, String)>,
    errors: Vec<String>,
}

// =============================================================================
// Default Ignore Rules
// =============================================================================

const DEFAULT_IGNORES: &[&str] = &[
    "target",
    "node_modules",
    "dist",
    "build",
    "vendor",
    "skills",
];

// =============================================================================
// Justfile Template
// =============================================================================

const JUSTFILE_TEMPLATE: &str = r#"# https://github.com/mizchi/moonbit-template
# SPDX-License-Identifier: MIT
# MoonBit Project Commands

target := "js"

default: check test

fmt:
    moon fmt

check:
    moon check --deny-warn --target {{target}}

test:
    moon test --target {{target}}

test-update:
    moon test --update --target {{target}}

run:
    moon run src/main --target {{target}}

info:
    moon info

clean:
    moon clean

release-check: fmt info check test
"#;

// =============================================================================
// Core Logic
// =============================================================================

fn main() -> ExitCode {
    let raw_args: Vec<String> = std::env::args().collect();
    let command_hint = infer_command_hint(&raw_args);
    let root_span_name = format!("cli.{command_hint}");
    let cwd = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .display()
        .to_string();

    let root_attrs = RootAttrs {
        cli_command: command_hint,
        cli_args: mask_cli_args(&raw_args.iter().skip(1).cloned().collect::<Vec<_>>()),
        cwd,
    };
    let mut telemetry = telemetry::init(&root_span_name, root_attrs);

    let run_result = telemetry_span::with_span(
        &telemetry,
        telemetry.root_span_name(),
        telemetry.root_attributes(),
        || run_entrypoint(&telemetry, &raw_args),
    );

    let exit = match run_result {
        Ok(code) => code,
        Err(EntryError::Parse(parse_err)) => {
            let _ = parse_err.print();
            exit_code_from_i32(parse_err.exit_code())
        }
        Err(EntryError::Runtime(e)) => {
            eprintln!("Error: {e:#}");
            ExitCode::from(1)
        }
    };

    telemetry.shutdown();
    exit
}

enum EntryError {
    Parse(clap::Error),
    Runtime(anyhow::Error),
}

impl std::fmt::Display for EntryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EntryError::Parse(err) => write!(f, "{err}"),
            EntryError::Runtime(err) => write!(f, "{err}"),
        }
    }
}

fn run_entrypoint(
    telemetry: &Telemetry,
    raw_args: &[String],
) -> std::result::Result<ExitCode, EntryError> {
    let cli = telemetry_span::with_span(telemetry, "parse_args", &[], || {
        Cli::try_parse_from(raw_args.iter().cloned()).map_err(EntryError::Parse)
    })?;

    let success = run(telemetry, cli).map_err(EntryError::Runtime)?;
    if success {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn run(telemetry: &Telemetry, cli: Cli) -> Result<bool> {
    telemetry_span::with_span(telemetry, "load_config", &[], || {
        check_moon_available()?;
        Ok::<(), anyhow::Error>(())
    })?;

    match cli.command {
        Commands::Scan { common, json } => cmd_scan(telemetry, common, json),
        Commands::Apply {
            common,
            skip_update,
            repeat,
            packages,
            fail_fast,
            no_justfile,
            justfile_mode,
        } => cmd_apply(
            telemetry,
            common,
            skip_update,
            repeat,
            packages,
            fail_fast,
            !no_justfile,
            justfile_mode,
        ),
        Commands::Just { common, mode } => cmd_just(telemetry, common, mode),
    }
}

fn infer_command_hint(args: &[String]) -> String {
    for arg in args.iter().skip(1) {
        if arg.starts_with('-') {
            continue;
        }
        return match arg.as_str() {
            "scan" | "apply" | "just" => arg.clone(),
            _ => "unknown".to_string(),
        };
    }
    "unknown".to_string()
}

fn input_path_attr(path: &Path) -> KeyValue {
    KeyValue::new("input_path", path.display().to_string())
}

fn exit_code_from_i32(code: i32) -> ExitCode {
    if (0..=255).contains(&code) {
        ExitCode::from(code as u8)
    } else {
        ExitCode::from(1)
    }
}

/// Get the moon binary path, checking common installation locations
fn get_moon_bin() -> PathBuf {
    // First check if moon is in PATH
    if let Ok(output) = Command::new("moon").arg("version").output() {
        if output.status.success() {
            return PathBuf::from("moon");
        }
    }

    // Check ~/.moon/bin/moon (common MoonBit installation path)
    if let Some(home) = std::env::var_os("HOME") {
        let moon_path = PathBuf::from(home).join(".moon/bin/moon");
        if moon_path.exists() {
            return moon_path;
        }
    }

    // Fallback to "moon" and let it fail with a clear error
    PathBuf::from("moon")
}

fn check_moon_available() -> Result<()> {
    let moon_bin = get_moon_bin();
    let output = Command::new(&moon_bin).arg("version").output();

    match output {
        Ok(o) if o.status.success() => Ok(()),
        _ => bail!(
            "'moon' CLI not found. Checked PATH and ~/.moon/bin/moon. Please install MoonBit first."
        ),
    }
}

// =============================================================================
// Scan Command
// =============================================================================

fn cmd_scan(telemetry: &Telemetry, common: CommonOptions, json_output: bool) -> Result<bool> {
    let repos = telemetry_span::with_span(
        telemetry,
        "load_inputs",
        &[input_path_attr(&common.root)],
        || discover_repos(&common),
    )?;

    if json_output {
        let output_json = telemetry_span::with_span(telemetry, "main_operation", &[], || {
            let output = ScanOutput {
                repos: repos
                    .iter()
                    .map(|r| RepoOutput {
                        repo_root: r.root.display().to_string(),
                        moon_mods: r
                            .moon_mods
                            .iter()
                            .map(|m| MoonModOutput {
                                path: m
                                    .path
                                    .strip_prefix(&r.root)
                                    .unwrap_or(&m.path)
                                    .display()
                                    .to_string(),
                                deps: m.deps.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
            };
            Ok::<String, anyhow::Error>(serde_json::to_string_pretty(&output)?)
        })?;

        telemetry_span::with_span(telemetry, "write_outputs", &[], || {
            println!("{output_json}");
            Ok::<(), anyhow::Error>(())
        })?;
    } else {
        let lines = telemetry_span::with_span(telemetry, "main_operation", &[], || {
            let mut lines = Vec::new();
            for repo in &repos {
                lines.push(format!("Repository: {}", repo.root.display()));
                for moon_mod in &repo.moon_mods {
                    let rel_path = moon_mod
                        .path
                        .strip_prefix(&repo.root)
                        .unwrap_or(&moon_mod.path);
                    lines.push(format!("  {}", rel_path.display()));
                    for dep in &moon_mod.deps {
                        lines.push(format!("    - {dep}"));
                    }
                }
                lines.push(String::new());
            }

            let total_mods: usize = repos.iter().map(|r| r.moon_mods.len()).sum();
            let total_deps: usize = repos
                .iter()
                .flat_map(|r| &r.moon_mods)
                .map(|m| m.deps.len())
                .sum();

            lines.push(format!(
                "Summary: {} repos, {} moon.mod.json files, {} dependencies",
                repos.len(),
                total_mods,
                total_deps
            ));

            Ok::<Vec<String>, anyhow::Error>(lines)
        })?;

        telemetry_span::with_span(telemetry, "write_outputs", &[], || {
            for line in &lines {
                println!("{line}");
            }
            Ok::<(), anyhow::Error>(())
        })?;
    }

    Ok(true)
}

// =============================================================================
// Apply Command
// =============================================================================

fn cmd_apply(
    telemetry: &Telemetry,
    common: CommonOptions,
    skip_update: bool,
    repeat: u32,
    packages: Vec<String>,
    fail_fast: bool,
    write_justfile: bool,
    justfile_mode: JustfileMode,
) -> Result<bool> {
    let repos = telemetry_span::with_span(
        telemetry,
        "load_inputs",
        &[input_path_attr(&common.root)],
        || discover_repos(&common),
    )?;

    if repos.is_empty() {
        telemetry_span::with_span(telemetry, "write_outputs", &[], || {
            println!("No moon.mod.json files found.");
            Ok::<(), anyhow::Error>(())
        })?;
        return Ok(true);
    }

    let results = telemetry_span::with_span(telemetry, "main_operation", &[], || {
        // Configure thread pool
        let jobs = common.jobs.unwrap_or_else(|| num_cpus::get() / 2).max(1);
        rayon::ThreadPoolBuilder::new()
            .num_threads(jobs)
            .build_global()
            .ok(); // Ignore if already initialized

        let verbose = common.verbose;
        let dry_run = common.dry_run;

        // Track if we should stop early
        let should_stop = AtomicBool::new(false);
        let results: Mutex<Vec<RepoResult>> = Mutex::new(Vec::new());

        repos.par_iter().for_each(|repo| {
            if fail_fast && should_stop.load(Ordering::Relaxed) {
                return;
            }

            let result = process_repo(
                repo,
                skip_update,
                repeat,
                &packages,
                dry_run,
                verbose,
                write_justfile,
                justfile_mode,
            );

            let success = result.success;
            results.lock().unwrap().push(result);

            if fail_fast && !success {
                should_stop.store(true, Ordering::Relaxed);
            }
        });

        Ok::<Vec<RepoResult>, anyhow::Error>(results.into_inner().unwrap())
    })?;

    telemetry_span::with_span(telemetry, "write_outputs", &[], || {
        println!("\n=== Results ===\n");
        for result in &results {
            let status = if result.success { "OK" } else { "FAILED" };
            println!("[{status}] {}", result.repo_root.display());

            if !result.updated_packages.is_empty() {
                println!("  Updated: {} packages", result.updated_packages.len());
            }

            if !result.failed_packages.is_empty() {
                println!("  Failed packages:");
                for (pkg, err) in &result.failed_packages {
                    println!("    - {pkg}: {err}");
                }
            }

            for err in &result.errors {
                println!("  Error: {err}");
            }
        }

        let success_count = results.iter().filter(|r| r.success).count();
        println!(
            "\nSummary: {}/{} repos succeeded",
            success_count,
            results.len()
        );
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(results.iter().all(|r| r.success))
}

fn process_repo(
    repo: &RepoInfo,
    skip_update: bool,
    repeat: u32,
    filter_packages: &[String],
    dry_run: bool,
    verbose: bool,
    write_justfile: bool,
    justfile_mode: JustfileMode,
) -> RepoResult {
    let mut result = RepoResult {
        repo_root: repo.root.clone(),
        success: true,
        updated_packages: Vec::new(),
        failed_packages: Vec::new(),
        errors: Vec::new(),
    };

    // 1. Run moon update (unless skipped)
    if !skip_update {
        if verbose || dry_run {
            println!("[{}] moon update", repo.root.display());
        }
        if !dry_run {
            match run_moon_command(&["update"], &repo.root) {
                Ok(_) => {
                    if verbose {
                        println!("[{}] moon update succeeded", repo.root.display());
                    }
                }
                Err(e) => {
                    result.errors.push(format!("moon update failed: {e}"));
                    result.success = false;
                    return result;
                }
            }
        }
    }

    // 2. Collect all deps from all moon.mod.json files
    let all_deps: Vec<String> = repo
        .moon_mods
        .iter()
        .flat_map(|m| m.deps.iter())
        .filter(|dep| filter_packages.is_empty() || filter_packages.iter().any(|p| dep.contains(p)))
        .cloned()
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    // 3. Run moon add for each package (repeated as specified)
    for _ in 0..repeat {
        for dep in &all_deps {
            if verbose || dry_run {
                println!("[{}] moon add {}", repo.root.display(), dep);
            }
            if !dry_run {
                match run_moon_command(&["add", dep], &repo.root) {
                    Ok(_) => {
                        if !result.updated_packages.contains(dep) {
                            result.updated_packages.push(dep.clone());
                        }
                    }
                    Err(e) => {
                        result.failed_packages.push((dep.clone(), e.to_string()));
                        result.success = false;
                    }
                }
            }
        }
    }

    // 4. Handle justfile
    if write_justfile {
        if let Err(e) = handle_justfile(&repo.root, justfile_mode, dry_run, verbose) {
            result.errors.push(format!("justfile handling failed: {e}"));
        }
    }

    result
}

fn run_moon_command(args: &[&str], cwd: &Path) -> Result<String> {
    let moon_bin = get_moon_bin();
    let output = Command::new(&moon_bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("Failed to execute moon {}", args.join(" ")))?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        bail!("exit code {code}: {stderr}")
    }
}

// =============================================================================
// Just Command
// =============================================================================

fn cmd_just(telemetry: &Telemetry, common: CommonOptions, mode: JustfileMode) -> Result<bool> {
    let repos = telemetry_span::with_span(
        telemetry,
        "load_inputs",
        &[input_path_attr(&common.root)],
        || discover_repos(&common),
    )?;

    if repos.is_empty() {
        telemetry_span::with_span(telemetry, "write_outputs", &[], || {
            println!("No moon.mod.json files found.");
            Ok::<(), anyhow::Error>(())
        })?;
        return Ok(true);
    }

    let (success_count, skip_count) =
        telemetry_span::with_span(telemetry, "main_operation", &[], || {
            let dry_run = common.dry_run;
            let verbose = common.verbose;
            let mut success_count = 0;
            let mut skip_count = 0;

            for repo in &repos {
                match handle_justfile(&repo.root, mode, dry_run, verbose) {
                    Ok(created) => {
                        if created {
                            success_count += 1;
                        } else {
                            skip_count += 1;
                        }
                    }
                    Err(e) => {
                        eprintln!("[{}] Error: {e}", repo.root.display());
                    }
                }
            }

            Ok::<(usize, usize), anyhow::Error>((success_count, skip_count))
        })?;

    telemetry_span::with_span(telemetry, "write_outputs", &[], || {
        println!("\nSummary: {success_count} created, {skip_count} skipped");
        Ok::<(), anyhow::Error>(())
    })?;

    Ok(true)
}

fn handle_justfile(
    repo_root: &Path,
    mode: JustfileMode,
    dry_run: bool,
    verbose: bool,
) -> Result<bool> {
    let justfile_path = repo_root.join("justfile");
    let exists = justfile_path.exists();

    match mode {
        JustfileMode::Skip => {
            if verbose {
                println!("[{}] Skipping justfile (skip mode)", repo_root.display());
            }
            Ok(false)
        }
        JustfileMode::Create => {
            if exists {
                if verbose {
                    println!(
                        "[{}] justfile already exists, skipping",
                        repo_root.display()
                    );
                }
                Ok(false)
            } else {
                if verbose || dry_run {
                    println!("[{}] Creating justfile", repo_root.display());
                }
                if !dry_run {
                    std::fs::write(&justfile_path, JUSTFILE_TEMPLATE)
                        .with_context(|| format!("Failed to write {}", justfile_path.display()))?;
                }
                Ok(true)
            }
        }
        JustfileMode::Merge => {
            // Merge mode is not implemented as per spec (just mentioned)
            if verbose {
                println!("[{}] Merge mode not implemented", repo_root.display());
            }
            Ok(false)
        }
    }
}

// =============================================================================
// Discovery Logic
// =============================================================================

fn discover_repos(common: &CommonOptions) -> Result<Vec<RepoInfo>> {
    let root = common
        .root
        .canonicalize()
        .with_context(|| format!("Invalid root path: {}", common.root.display()))?;

    // Build ignore list
    let mut ignores: Vec<String> = common.ignores.clone();
    if !common.no_default_ignore {
        ignores.extend(DEFAULT_IGNORES.iter().map(|s| s.to_string()));
    }

    // Find all moon.mod.json files
    let moon_mods = find_moon_mods(&root, &ignores, common.verbose)?;

    // Group by repo root
    let mut repo_map: HashMap<PathBuf, Vec<MoonModInfo>> = HashMap::new();
    for moon_mod in moon_mods {
        let repo_root = find_repo_root(&moon_mod.path);
        repo_map.entry(repo_root).or_default().push(moon_mod);
    }

    // Convert to Vec<RepoInfo>
    let mut repos: Vec<RepoInfo> = repo_map
        .into_iter()
        .map(|(root, moon_mods)| RepoInfo { root, moon_mods })
        .collect();

    // Sort for consistent output
    repos.sort_by(|a, b| a.root.cmp(&b.root));

    Ok(repos)
}

fn find_moon_mods(root: &Path, ignores: &[String], verbose: bool) -> Result<Vec<MoonModInfo>> {
    let mut moon_mods = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !should_ignore(e.path(), ignores))
    {
        let entry = entry?;
        if entry.file_type().is_file() && entry.file_name() == "moon.mod.json" {
            let path = entry.path().to_path_buf();
            match parse_moon_mod(&path) {
                Ok(deps) => {
                    if verbose {
                        println!("Found: {}", path.display());
                    }
                    moon_mods.push(MoonModInfo { path, deps });
                }
                Err(e) => {
                    eprintln!("Warning: Failed to parse {}: {e}", path.display());
                }
            }
        }
    }

    Ok(moon_mods)
}

fn should_ignore(path: &Path, ignores: &[String]) -> bool {
    for component in path.components() {
        if let std::path::Component::Normal(name) = component {
            let name_str = name.to_string_lossy();
            // Ignore dotfiles/dotfolders (starting with '.')
            if name_str.starts_with('.') {
                return true;
            }
            for ignore in ignores {
                if name_str == *ignore {
                    return true;
                }
            }
        }
    }
    false
}

fn parse_moon_mod(path: &Path) -> Result<Vec<String>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read {}", path.display()))?;

    let moon_mod: MoonMod = serde_json::from_str(&content)
        .with_context(|| format!("Failed to parse {}", path.display()))?;

    let mut deps: Vec<String> = moon_mod.deps.keys().cloned().collect();
    deps.sort();
    Ok(deps)
}

fn find_repo_root(moon_mod_path: &Path) -> PathBuf {
    let dir = moon_mod_path.parent().unwrap_or(moon_mod_path);

    // Walk up looking for .git
    let mut current = dir;
    loop {
        if current.join(".git").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => break,
        }
    }

    // No .git found, use the moon.mod.json's directory
    dir.to_path_buf()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_ignore() {
        let ignores = vec![".git".to_string(), "node_modules".to_string()];

        assert!(should_ignore(Path::new("/foo/.git/config"), &ignores));
        assert!(should_ignore(Path::new("/foo/node_modules/bar"), &ignores));
        assert!(!should_ignore(Path::new("/foo/src/main.rs"), &ignores));
    }

    #[test]
    fn test_parse_moon_mod() {
        let json = r#"{"deps": {"moonbitlang/core": "0.1.0", "moonbitlang/x": "0.2.0"}}"#;
        let temp_dir = std::env::temp_dir();
        let temp_file = temp_dir.join("test_moon_mod.json");
        std::fs::write(&temp_file, json).unwrap();

        let deps = parse_moon_mod(&temp_file).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"moonbitlang/core".to_string()));
        assert!(deps.contains(&"moonbitlang/x".to_string()));

        std::fs::remove_file(temp_file).ok();
    }

    #[test]
    fn test_should_ignore_dotfiles() {
        let ignores = vec![];
        assert!(should_ignore(Path::new("/foo/.mooncakes/bar"), &ignores));
        assert!(should_ignore(Path::new("/foo/.git/config"), &ignores));
        assert!(should_ignore(Path::new("/foo/.hidden"), &ignores));
        assert!(!should_ignore(Path::new("/foo/src/main.rs"), &ignores));
    }

    #[test]
    fn test_infer_command_hint() {
        assert_eq!(
            infer_command_hint(&["moon-dst".to_string(), "scan".to_string()]),
            "scan"
        );
        assert_eq!(
            infer_command_hint(&["moon-dst".to_string(), "--help".to_string()]),
            "unknown"
        );
        assert_eq!(
            infer_command_hint(&["moon-dst".to_string(), "foo".to_string()]),
            "unknown"
        );
    }
}
