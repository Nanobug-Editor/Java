//! Java owns JDT debug discovery and a private loopback DAP bridge; the host owns the process tree.
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::{BufReader, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::Path,
    sync::mpsc,
    time::{Duration, Instant},
};

/// Start one private language runtime, resolve its debug port and relay byte-exact DAP over stdio.
/// No port is accepted from project configuration. Every exit path terminates the owned backend.
pub(crate) fn run(options: &BTreeMap<String, String>) -> Result<()> {
    let (mut child, _index) = super::spawn_language(options)?;
    let result = bridge(
        options,
        child.stdin.take().unwrap(),
        child.stdout.take().unwrap(),
    );
    let _ = child.kill();
    let _ = child.wait();
    result
}

/// Initialization uses the same plain-source project settings as editing, with all importers off.
fn bridge(
    options: &BTreeMap<String, String>,
    mut input: std::process::ChildStdin,
    output: std::process::ChildStdout,
) -> Result<()> {
    let workspace = Path::new(
        options
            .get("--workspace")
            .context("Missing debug workspace")?,
    )
    .canonicalize()?;
    let root = url::Url::from_directory_path(&workspace)
        .map_err(|_| anyhow::anyhow!("Invalid debug workspace URI"))?
        .to_string();
    let project_argument = options.get("--project").context("Missing debug project")?;
    let encoded_project = if project_argument == "${nanobug:project}" {
        std::env::var("NANOBUG_JAVA_PROJECT").context("Missing prepared Java debug project")?
    } else {
        project_argument.clone()
    };
    ensure!(
        encoded_project.len() <= 32768,
        "Java debug project metadata exceeds its budget"
    );
    let project: super::project::Project = serde_json::from_str(&encoded_project)?;
    project.validate().map_err(anyhow::Error::msg)?;
    let home = options
        .get("--project-jdk")
        .context("Missing debug project JDK")?;
    let resources = Path::new(
        options
            .get("--resources")
            .context("Missing debug resources")?,
    );
    let settings = json!({"java":{
        "project":{"sourcePaths":project.source_roots,"referencedLibraries":project.jars,"outputPath":project.output},
        "import":{"maven":{"enabled":false},"gradle":{"enabled":false}},
        "maven":{"downloadSources":false},"eclipse":{"downloadSources":false},
        "configuration":{"runtimes":[{"name":options.get("--execution-environment").context("Missing execution environment")?,"path":home,"default":true}]}
    }});
    let (send, receive) = mpsc::sync_channel(32);
    std::thread::Builder::new()
        .name("java-debug-discovery".into())
        .spawn(move || {
            let mut output = BufReader::new(output);
            while let Ok(Some(message)) = super::lsp::read_frame(&mut output) {
                if send.send(message).is_err() {
                    break;
                }
            }
        })?;
    super::lsp::write_frame(
        &mut input,
        &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
            "processId":std::process::id(),"rootUri":root,"workspaceFolders":[{"uri":root,"name":"Java debug"}],
            "capabilities":{"workspace":{"configuration":true}},
            "initializationOptions":{"settings":settings,"bundles":[resources.join("native/java-diagnostics.jar"),resources.join("native/java-debug.jar")]}
        }}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(120);
    let port = loop {
        ensure!(
            Instant::now() < deadline,
            "Java debugger discovery timed out"
        );
        let message = match receive.recv_timeout(Duration::from_millis(250)) {
            Ok(message) => message,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => bail!("Java debug backend exited during initialization"),
        };
        if message["id"] == 1 && message.get("method").is_none() {
            ensure!(
                message["error"].is_null(),
                "Java debug initialization failed: {}",
                message["error"]
            );
            super::lsp::write_frame(
                &mut input,
                &json!({"jsonrpc":"2.0","method":"initialized","params":{}}),
            )?;
            super::lsp::write_frame(
                &mut input,
                &json!({"jsonrpc":"2.0","id":2,"method":"workspace/executeCommand","params":{"command":"vscode.java.startDebugSession","arguments":[]}}),
            )?;
        } else if message["id"] == 2 && message.get("method").is_none() {
            ensure!(
                message["error"].is_null(),
                "Java debug discovery failed: {}",
                message["error"]
            );
            break message["result"]
                .as_u64()
                .and_then(|port| u16::try_from(port).ok())
                .filter(|port| *port != 0)
                .context("Java debugger returned an invalid port")?;
        } else {
            respond(&mut input, &message, &settings, &root)?;
        }
    };
    // Keep serving the backend's ordinary configuration requests while DAP runs independently.
    std::thread::Builder::new()
        .name("java-debug-index".into())
        .spawn(move || {
            while let Ok(message) = receive.recv() {
                if respond(&mut input, &message, &settings, &root).is_err() {
                    break;
                }
            }
        })?;
    let mut connection = TcpStream::connect_timeout(
        &SocketAddrV4::new(Ipv4Addr::LOCALHOST, port).into(),
        Duration::from_secs(10),
    )?;
    connection.set_nodelay(true)?;
    let mut writer = connection.try_clone()?;
    std::thread::Builder::new()
        .name("java-debug-input".into())
        .spawn(move || {
            let _ = std::io::copy(&mut std::io::stdin().lock(), &mut writer);
            let _ = writer.shutdown(std::net::Shutdown::Write);
        })?;
    let mut output = std::io::stdout().lock();
    let mut buffer = [0; 16 * 1024];
    loop {
        let length = connection.read(&mut buffer)?;
        if length == 0 {
            break;
        }
        output.write_all(&buffer[..length])?;
        output.flush()?;
    }
    Ok(())
}

/// Reply only to standard backend requests; this bridge never applies workspace edits or commands.
fn respond(
    input: &mut std::process::ChildStdin,
    message: &Value,
    settings: &Value,
    root: &str,
) -> Result<()> {
    if message.get("id").is_none() || message.get("method").is_none() {
        return Ok(());
    }
    let result = match message["method"].as_str().unwrap_or("") {
        "workspace/configuration" => Value::Array(
            message["params"]["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|item| {
                    let mut value = settings.clone();
                    for part in item["section"]
                        .as_str()
                        .unwrap_or("")
                        .split('.')
                        .filter(|part| !part.is_empty())
                    {
                        value = value[part].clone();
                    }
                    value
                })
                .collect(),
        ),
        "workspace/workspaceFolders" => json!([{"uri":root,"name":"Java debug"}]),
        "workspace/applyEdit" => {
            json!({"applied":false,"failureReason":"Debug discovery cannot edit workspace resources"})
        }
        _ => Value::Null,
    };
    super::lsp::write_frame(
        input,
        &json!({"jsonrpc":"2.0","id":message["id"],"result":result}),
    )
}
