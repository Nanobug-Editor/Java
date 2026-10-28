//! A bounded native javac build retains checked source/output paths for the owned child lifetime.
use super::*;
use anyhow::ensure;
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
};

/// Compile the complete admitted source set using the selected project SDK. Native execution owns
/// this adapter and javac in one process tree; literal arguments never pass through a Shell.
pub(crate) fn run(options: &BTreeMap<String, String>) -> Result<()> {
    let root = PathBuf::from(required(options, "--workspace")?).canonicalize()?;
    let project: project::Project = serde_json::from_str(required(options, "--project")?)?;
    project.validate().map_err(anyhow::Error::msg)?;
    let jdk = PathBuf::from(required(options, "--jdk")?).canonicalize()?;
    let compiler = jdk.join("bin").join("javac.exe");
    ensure!(compiler.is_file(), "The project SDK has no Java compiler");
    let mut pins = Vec::new();
    // Pin existing ancestors as well as the workspace so a rename cannot redirect the command.
    for directory in root.ancestors().collect::<Vec<_>>().into_iter().rev() {
        pins.push(pin_directory(directory)?);
    }
    let output = output_directory(&root, &project.output, &mut pins)?;
    let mut sources = Vec::new();
    let mut budget = Budget {
        entries: 0,
        bytes: 0,
    };
    for source in &project.source_roots {
        let path = root.join(source);
        let path = path
            .canonicalize()
            .with_context(|| format!("Missing Java source directory: {source}"))?;
        ensure!(
            path.starts_with(&root) && path.is_dir() && !path.starts_with(&output),
            "Source directory must remain inside the workspace and outside compiler output"
        );
        collect(&path, &output, 0, &mut budget, &mut sources, &mut pins)?;
    }
    sources.sort();
    sources.dedup();
    ensure!(
        !sources.is_empty(),
        "No Java sources found in the configured source directories"
    );
    let mut classpath = vec![output.clone()];
    for jar in &project.jars {
        let jar = root
            .join(jar)
            .canonicalize()
            .with_context(|| format!("Missing local dependency: {jar}"))?;
        ensure!(jar.is_file(), "Local dependency is not a JAR file");
        pins.push(pin_source(&jar)?);
        classpath.push(jar);
    }
    let classpath = std::env::join_paths(classpath.iter().map(|path| java_path(path)))?;
    let mut args = vec![
        OsString::from("-J-Dfile.encoding=UTF-8"),
        // Compiler output uses pipes. Set both older and current JVM stream properties so
        // localized messages and source paths remain UTF-8 independently of the system code page.
        "-J-Dsun.stdout.encoding=UTF-8".into(),
        "-J-Dsun.stderr.encoding=UTF-8".into(),
        "-J-Dstdout.encoding=UTF-8".into(),
        "-J-Dstderr.encoding=UTF-8".into(),
        "-g".into(),
        "-encoding".into(),
        "UTF-8".into(),
        "-d".into(),
        java_path(&output).into_os_string(),
        "-classpath".into(),
        classpath,
    ];
    args.extend(sources.iter().map(|path| java_path(path).into_os_string()));
    // Keep one complete bounded invocation. Oversized projects fail before any compiler effects.
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let units = compiler.as_os_str().encode_wide().count() * 2
            + 3
            + args
                .iter()
                .map(|argument| argument.encode_wide().count() * 2 + 3)
                .sum::<usize>();
        ensure!(
            units <= 30_000,
            "Java compiler argument limit exceeded; narrow the configured source directories"
        );
    }
    let status = Command::new(java_path(&compiler))
        .args(&args)
        .current_dir(java_path(&root))
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .with_context(|| format!("Cannot start compiler: {}", compiler.display()))?;
    ensure!(status.success(), "Java compilation failed: {status}");
    Ok(())
}

/// JDK 11's NIO parser rejects verbatim prefixes in argv. Convert only the native representation
/// after canonical containment and lifetime pins are established; preserve UTF-16 and UNC identity.
fn java_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let units: Vec<u16> = path.as_os_str().encode_wide().collect();
        let verbatim: Vec<u16> = r"\\?\".encode_utf16().collect();
        let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        if let Some(rest) = units.strip_prefix(unc.as_slice()) {
            let mut normalized: Vec<u16> = r"\\".encode_utf16().collect();
            normalized.extend_from_slice(rest);
            return PathBuf::from(OsString::from_wide(&normalized));
        }
        if let Some(rest) = units.strip_prefix(verbatim.as_slice()) {
            return PathBuf::from(OsString::from_wide(rest));
        }
    }
    path.to_path_buf()
}

/// Counters bound traversal independently from native argv and the source-file count.
struct Budget {
    entries: usize,
    bytes: u64,
}

/// Reject links/special files and keep original source handles so javac reads coherent disk bytes.
fn collect(
    path: &Path,
    output: &Path,
    depth: usize,
    budget: &mut Budget,
    sources: &mut Vec<PathBuf>,
    pins: &mut Vec<File>,
) -> Result<()> {
    ensure!(depth <= 64, "Java source depth exceeded");
    pins.push(pin_directory(path)?);
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        budget.entries += 1;
        ensure!(
            budget.entries <= 10_000,
            "Java source traversal limit exceeded"
        );
        let path = entry.path();
        if path == output {
            continue;
        }
        let metadata = fs::symlink_metadata(&path)?;
        ensure!(!linked(&metadata), "Java sources cannot traverse links");
        if metadata.is_dir() {
            if entry.file_name().to_string_lossy().starts_with('.') {
                continue;
            }
            collect(&path, output, depth + 1, budget, sources, pins)?;
        } else if metadata.is_file()
            && path
                .extension()
                .is_some_and(|extension| extension == "java")
        {
            budget.bytes += metadata.len();
            ensure!(
                sources.len() < 512 && budget.bytes <= 64 * 1024 * 1024,
                "Java source size limit exceeded"
            );
            pins.push(pin_source(&path)?);
            sources.push(path);
        }
    }
    Ok(())
}

/// Create each owned output segment only under retained real parents; never follow a junction.
fn output_directory(root: &Path, relative: &str, pins: &mut Vec<File>) -> Result<PathBuf> {
    let mut path = root.to_owned();
    for part in relative.replace('\\', "/").split('/') {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(metadata) => ensure!(
                metadata.is_dir() && !linked(&metadata),
                "Compiler output contains a link or file"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&path)?,
            Err(error) => return Err(error.into()),
        }
        pins.push(pin_directory(&path)?);
    }
    let path = path.canonicalize()?;
    ensure!(
        path.starts_with(root),
        "Compiler output escapes the workspace"
    );
    Ok(path)
}

/// Windows directory handles exclude replacement while leaving child creation available to javac.
fn pin_directory(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(3).custom_flags(0x02000000 | 0x00200000);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_dir() && !linked(&metadata),
        "Java directory is not a stable real directory"
    );
    Ok(file)
}
/// Source handles exclude concurrent writers/deletion; native editor buffers remain independent.
fn pin_source(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x00200000);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && !linked(&metadata),
        "Java source/dependency is not a stable real file"
    );
    Ok(file)
}
/// Reparse points include Windows junctions, beyond portable symbolic-link metadata.
fn linked(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}
/// Missing options fail before source enumeration or output creation.
fn required<'a>(options: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    options
        .get(name)
        .map(String::as_str)
        .with_context(|| format!("Missing compiler option: {name}"))
}
