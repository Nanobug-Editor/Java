//! The owned Java launcher aligns its private Windows console with the JVM's UTF-8 byte streams.
use super::*;

/// Start the selected JVM with literal arguments and inherited controlled stdio. Console code pages
/// belong to this execution's ConPTY; changing them here never changes machine/user defaults.
/// Preserve the target's exit status so failed applications cannot appear as successful runs.
pub(crate) fn run(mut arguments: impl Iterator<Item = String>) -> Result<()> {
    anyhow::ensure!(
        arguments.next().as_deref() == Some("--java"),
        "Missing Java executable option"
    );
    let program = PathBuf::from(arguments.next().context("Missing Java executable")?);
    anyhow::ensure!(
        program.is_absolute() && program.is_file(),
        "Java executable is unavailable"
    );
    // Generic borrowed execution metadata is for the Java plugin's debugger, never JVM arguments.
    let mut boundary = arguments.next();
    let imported = boundary.as_deref() == Some("--context");
    if imported {
        let key = arguments.next().context("Missing Java context key")?;
        anyhow::ensure!(
            key.len() == 64 && key.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "Invalid Java context key"
        );
        anyhow::ensure!(
            arguments.next().as_deref() == Some("--jdk-version"),
            "Missing Java context JDK"
        );
        let jdk = arguments.next().context("Missing Java JDK feature")?;
        anyhow::ensure!(
            ["8", "11", "17", "21", "25"].contains(&jdk.as_str()),
            "Invalid Java JDK feature"
        );
        boundary = arguments.next();
    }
    anyhow::ensure!(
        boundary.as_deref() == Some("--"),
        "Missing literal Java argument boundary"
    );
    let mut args: Vec<_> = arguments.collect();
    anyhow::ensure!(
        args.len() <= 128
            && args
                .iter()
                .all(|value| value.len() <= 4096 && !value.contains('\0')),
        "Java launch arguments exceed their bounds"
    );
    if imported
        && args.get(3).is_some_and(|arg| arg == "-classpath")
        && args.get(4).is_some_and(|arg| arg == "${nanobug:classpath}")
    {
        let classpath = std::env::var("NANOBUG_JAVA_CLASSPATH")
            .context("Missing prepared Java classpath environment")?;
        anyhow::ensure!(
            !classpath.contains('\0') && classpath.len() <= 32768,
            "Invalid prepared Java classpath environment"
        );
        args[4] = classpath;
    }
    console_encoding()?;
    let status = Command::new(program)
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("Cannot start Java application")?;
    std::process::exit(status.code().unwrap_or(1));
}

/// Match the input/output console code pages before Java reads them during VM initialization.
/// Pipe-only launches have no console and already retain the explicitly selected JVM encoding.
fn console_encoding() -> Result<()> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleCP() -> u32;
            fn GetConsoleOutputCP() -> u32;
            fn SetConsoleCP(code_page: u32) -> i32;
            fn SetConsoleOutputCP(code_page: u32) -> i32;
        }
        // These calls address only the current process's attached console, not a global setting.
        if unsafe { GetConsoleCP() != 0 || GetConsoleOutputCP() != 0 } {
            anyhow::ensure!(
                unsafe { SetConsoleCP(65001) != 0 && SetConsoleOutputCP(65001) != 0 },
                "Cannot configure Java console encoding: {}",
                std::io::Error::last_os_error()
            );
        }
    }
    Ok(())
}
