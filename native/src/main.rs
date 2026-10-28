//! A controlled native adapter owns Java child processes and leaves document ownership to the host.
use anyhow::{Context, Result, bail};
mod compile;
mod debug;
mod index;
mod launch;
mod lsp;
#[path = "../../src/project.rs"]
mod project;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

/// Execute a declared adapter mode; failures go to stderr and never corrupt the language stream.
fn main() {
    if let Err(error) = run() {
        eprintln!("Java: {error:#}");
        std::process::exit(1);
    }
}

/// Parse literal argv pairs without interpreting shell text or accepting unknown adapter modes.
fn run() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let mode = args.next().context("Missing adapter mode")?;
    if mode == "run" {
        return launch::run(args);
    }
    let mut options = BTreeMap::new();
    while let Some(key) = args.next() {
        let value = args.next().context("Missing adapter option value")?;
        anyhow::ensure!(
            key.starts_with("--") && options.insert(key, value).is_none(),
            "Invalid adapter option"
        );
    }
    match mode.as_str() {
        "lsp" => language(&options),
        "compile" => compile::run(&options),
        "debug" => debug::run(&options),
        _ => bail!("Unsupported adapter mode: {mode}"),
    }
}

/// Launch JDT LS over inherited standard streams using private mutable configuration and index data.
/// `options` contains immutable package resources, private data, and an optional explicit Java tool.
fn language(options: &BTreeMap<String, String>) -> Result<()> {
    // Preserve this reader across startup: it may already contain later client frames.
    let mut source = std::io::BufReader::new(std::io::stdin());
    let first = lsp::read_frame(&mut source)?.context("Missing Java initialization")?;
    anyhow::ensure!(
        first["method"] == "initialize",
        "Java initialization must be first"
    );
    let mut options = options.clone();
    if let Some(graph) = first["params"]["initializationOptions"].get("nanobugProjects") {
        let graph: project::Graph = serde_json::from_value(graph.clone())?;
        graph.validate().map_err(anyhow::Error::msg)?;
        let graph = serde_json::to_string(&graph)?;
        anyhow::ensure!(graph.len() <= 32768, "Java analysis graph quota exceeded");
        options.insert("--analysis-graph".into(), graph);
    }
    let (mut child, _index) = spawn_language(&options)?;
    let transport = lsp::relay(
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
        source,
        first,
    );
    if transport.is_err() {
        let _ = child.kill();
    }
    let status = child.wait()?;
    transport?;
    anyhow::ensure!(status.success(), "Java language server exited: {status}");
    Ok(())
}

/// Each analysis or debug incarnation has a private index and a controlled child JVM.
/// The caller owns cleanup; mutable configuration is never written into package/workspace resources.
fn spawn_language(
    options: &BTreeMap<String, String>,
) -> Result<(std::process::Child, index::IndexGuard)> {
    let server = PathBuf::from(
        options
            .get("--server")
            .context("Missing server directory")?,
    )
    .canonicalize()?;
    let data = PathBuf::from(
        options
            .get("--data")
            .context("Missing private data directory")?,
    );
    std::fs::create_dir_all(&data)?;
    let index = index::IndexGuard::acquire(&data)?;
    let graph = options
        .get("--analysis-graph")
        .map(|content| {
            let path = data.join("project-graph.json");
            std::fs::write(&path, content)?;
            Ok::<_, std::io::Error>(path)
        })
        .transpose()?;
    let config = data.join("configuration");
    let platform = if cfg!(windows) {
        "config_win"
    } else if cfg!(target_os = "macos") {
        "config_mac"
    } else {
        "config_linux"
    };
    copy_configuration(&server.join(platform), &config)?;
    let mut launchers = std::fs::read_dir(server.join("plugins"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with("org.eclipse.equinox.launcher_")
            })
        })
        .filter(|path| path.extension().is_some_and(|extension| extension == "jar"))
        .collect::<Vec<_>>();
    anyhow::ensure!(launchers.len() == 1, "Expected exactly one JDT launcher");
    let java = options.get("--java").map(String::as_str).unwrap_or("java");
    let mut command = Command::new(java);
    if let Some(graph) = graph {
        command.arg(format!("-Dnanobug.java.graph={}", graph.display()));
    }
    if options
        .get("--explicit-project")
        .is_some_and(|value| value == "true")
    {
        command.arg("-Dnanobug.java.explicitProject=true");
    }
    command
        .args([
            "-Declipse.application=org.eclipse.jdt.ls.core.id1",
            "-Dosgi.bundles.defaultStartLevel=4",
            "-Declipse.product=org.eclipse.jdt.ls.core.product",
            "-Dlog.level=WARNING",
            // IndexGuard already excludes another backend. Eclipse byte-range locks would prevent
            // the host's disposable preview snapshot while the old instance remains usable.
            "-Dosgi.locking=none",
            "-Xmx1G",
            "--add-modules=ALL-SYSTEM",
            "--add-opens",
            "java.base/java.util=ALL-UNNAMED",
            "--add-opens",
            "java.base/java.lang=ALL-UNNAMED",
            "-jar",
        ])
        .arg(launchers.remove(0))
        .arg("-configuration")
        .arg(&config)
        .arg("-data")
        .arg(data.join("workspace"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let child = command
        .spawn()
        .with_context(|| format!("Cannot start Java runtime: {java}"))?;
    Ok((child, index))
}

/// Copy immutable distribution configuration into private storage, rejecting links and special files.
/// `source` is the package configuration; `target` belongs to this service incarnation's private root.
fn copy_configuration(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            copy_configuration(&entry.path(), &target.join(entry.file_name()))?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), target.join(entry.file_name()))?;
        } else {
            bail!("Distribution configuration contains a link or special file");
        }
    }
    Ok(())
}
