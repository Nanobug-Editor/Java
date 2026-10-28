//! Java owns version selection; the host observes approved tools and prepares verified private artifacts.
use plugin_protocol::{
    api::{ErrorCode, Failure},
    dependencies, language, process,
};
use serde::Deserialize;
use serde_json::json;
use std::collections::BTreeMap;

/// An immutable distribution from the publisher's API snapshot, never a moving download label.
#[derive(Clone, Deserialize)]
struct Distribution {
    /// Java feature release required by the project.
    major: u16,
    /// Publisher's exact release identity.
    version: String,
    /// Verified distribution's top-level ZIP directory.
    folder: String,
    /// SHA-256 of the complete immutable download.
    sha256: String,
    /// Public HTTPS archive source, including an immutable release tag.
    url: String,
}

/// A local or managed JDK selection, with an artifact retained whenever cache files are referenced.
struct Jdk {
    /// Selected Java feature release.
    major: u16,
    /// Distribution directory or dependency placeholder during installation preparation.
    home: String,
    /// Concrete Java executable or dependency placeholder before private preparation.
    java: String,
    /// Managed distributions remain pinned by the language plan through process retirement.
    artifact: Option<dependencies::Artifact>,
}

/// Complete an owned language `plan` using `context` settings and readonly tool observations.
/// Returns typed configuration/permission failures; downloads run later in host preparation, never here.
pub(crate) fn prepare(
    context: &language::Context,
    plan: &mut language::Proposal,
) -> Result<(), Failure> {
    let target = context
        .settings
        .get("java.jdkVersion")
        .and_then(|value| value.value.as_str())
        .unwrap_or("25")
        .parse::<u16>()
        .map_err(|_| invalid("Invalid project JDK version"))?;
    let runtime = select_runtime(&context.settings)?;
    let project = if let Some(home) = context
        .settings
        .get("java.jdkHome")
        .and_then(|setting| setting.value.as_str())
        .filter(|home| !home.is_empty())
    {
        explicit_project(&context.settings, home, target)?
    } else if runtime.major == target && has_compiler(&runtime) {
        Jdk {
            major: runtime.major,
            home: runtime.home.clone(),
            java: runtime.java.clone(),
            artifact: runtime.artifact.clone(),
        }
    } else {
        select_project(target)?
    };
    // Candidate declarations contain the packager's verified native digest. No executable bytes
    // cross the fuel-bounded guest boundary merely to reconstruct that immutable declaration.
    let adapter = context
        .candidates
        .get("analysis")
        .and_then(|service| service.installation.clone())
        .ok_or_else(|| invalid("Missing packaged native adapter"))?;
    let mut artifacts: BTreeMap<String, dependencies::Artifact> = adapter
        .artifacts
        .into_iter()
        .map(|artifact| (artifact.id.clone(), artifact))
        .collect();
    for artifact in [runtime.artifact, project.artifact].into_iter().flatten() {
        artifacts.insert(artifact.id.clone(), artifact);
    }
    plan.program = None;
    plan.expand_dependencies = true;
    plan.installation = Some(dependencies::Plan {
        executable: "adapter/java-adapter.exe".into(),
        artifacts: artifacts.into_values().collect(),
    });
    let args = plan
        .args
        .as_mut()
        .ok_or_else(|| invalid("Missing language startup arguments"))?;
    let runtime_argument = args
        .iter()
        .position(|argument| argument == "--java")
        .ok_or_else(|| invalid("Missing language runtime argument"))?
        + 1;
    args[runtime_argument] = runtime.java;
    args.extend(["--project-jdk".into(), project.home.clone()]);
    // The host resolves explicit owned artifact paths after acquisition and before initialization.
    // Local SDK paths remain literal and are not interpreted as dependency templates.
    {
        let name = if project.major == 8 {
            "JavaSE-1.8".into()
        } else {
            format!("JavaSE-{}", project.major)
        };
        let settings = plan
            .configuration
            .as_mut()
            .and_then(|values| values.get_mut("java"))
            .ok_or_else(|| invalid("Missing Java configuration"))?;
        settings["configuration"] =
            json!({"runtimes":[{"name":name,"path":project.home,"default":true}]});
        plan.initialization_options = Some(json!({"settings":{"java":settings},
            "extendedClientCapabilities":{"classFileContentsSupport":true},
            "bundles":[super::native_path(&context.package_root, "native/java-diagnostics.jar")]}));
    }
    Ok(())
}

/// A machine-local project override is authoritative; explicit version requirements still apply.
/// `home` identifies a JDK directory, not a program argument or an installer source.
fn explicit_project(
    settings: &plugin_protocol::settings::Effective,
    home: &str,
    target: u16,
) -> Result<Jdk, Failure> {
    let source = process::ToolSource::Program {
        program: super::native_path(home, "bin/java.exe"),
    };
    let project = from_metadata(
        process::inspect_tool(source, 1, vec!["release".into()])?,
        None,
    )?;
    if ![8, 11, 17, 21, 25].contains(&project.major) {
        return Err(invalid(
            "The selected project JDK version is outside the supported versions",
        ));
    }
    let explicit_version = settings
        .get("java.jdkVersion")
        .is_some_and(|setting| setting.source != plugin_protocol::settings::Source::Default);
    if explicit_version && project.major != target {
        return Err(invalid(
            "The explicit project JDK does not match the project's requested version",
        ));
    }
    if !has_compiler(&project) {
        return Err(invalid(
            "The selected project JDK has no usable Java compiler",
        ));
    }
    Ok(project)
}

/// Return concrete project SDK tools for an owned run target without starting a language server.
/// A missing managed distribution remains a typed failure until its declared preparation completes.
pub(crate) fn prepared_project(
    settings: &plugin_protocol::settings::Effective,
) -> Result<(String, String), Failure> {
    let project = project_for(settings)?;
    if project.home.contains("${dependency:") {
        return Err(Failure::new(
            ErrorCode::NotFound,
            "The required project JDK has not been prepared",
        ));
    }
    Ok((project.home, project.java))
}

/// Narrow a cold method's immutable dependency allowlist to its adapter and any missing project SDK.
/// Local compatible tools need no download; explicit invalid overrides remain authoritative failures.
pub(crate) fn prepare_services(
    context: plugin_protocol::service::Preparation,
) -> Result<plugin_protocol::api::Output, Failure> {
    let debug = context.contract == "debug.session" && context.method == "start";
    if !debug
        && (context.contract != plugin_protocol::targets::CONTRACT || context.method != "prepare")
    {
        return Err(invalid("Unsupported Java native preparation"));
    }
    let settings = crate::imported::execution_settings(&context.settings, &context.arguments)?;
    let project = project_for(&settings)?;
    let mut services = vec!["analysis".to_owned()];
    if project.home.contains("${dependency:") {
        services.push(format!("managed{}", project.major));
    }
    if debug {
        let runtime = select_runtime(&settings)?;
        if runtime.home.contains("${dependency:") {
            let service = format!("managed{}", runtime.major);
            if !services.contains(&service) {
                services.push(service);
            }
        }
    }
    if services
        .iter()
        .any(|service| !context.services.contains(service))
    {
        return Err(invalid("Missing Java dependency preparation declaration"));
    }
    Ok(plugin_protocol::api::Output {
        service_preparation: Some(services),
        ..Default::default()
    })
}

/// Shared cold-run selection observes only approved metadata, including a managed cache hit.
fn project_for(settings: &plugin_protocol::settings::Effective) -> Result<Jdk, Failure> {
    let target = settings
        .get("java.jdkVersion")
        .and_then(|setting| setting.value.as_str())
        .unwrap_or("25")
        .parse::<u16>()
        .map_err(|_| invalid("Invalid project JDK version"))?;
    if let Some(home) = settings
        .get("java.jdkHome")
        .and_then(|setting| setting.value.as_str())
        .filter(|home| !home.is_empty())
    {
        explicit_project(settings, home, target)
    } else {
        select_project(target)
    }
}

/// A user's explicit runtime must work as specified; automatic discovery may use a managed fallback.
fn select_runtime(settings: &plugin_protocol::settings::Effective) -> Result<Jdk, Failure> {
    if let Some(program) = settings
        .get("java.runtime")
        .and_then(|setting| setting.value.as_str())
        .filter(|value| !value.is_empty())
    {
        let metadata = process::inspect_tool(
            process::ToolSource::Program {
                program: program.into(),
            },
            1,
            vec!["release".into()],
        )?;
        let runtime = from_metadata(metadata, None)?;
        if runtime.major < 21 {
            return Err(invalid(
                "The explicit language runtime requires Java 21 or newer",
            ));
        }
        return Ok(runtime);
    }
    for service in ["local-runtime", "local25", "local21"] {
        if let Ok(runtime) = observe(service, None) {
            if runtime.major >= 21 {
                return Ok(runtime);
            }
        }
    }
    managed(25)
}

/// Return the independently selected backend runtime and prepared project execution environment.
/// Native debugging must never replace a Java 8 project with the backend's newer JVM.
pub(crate) fn debug_tools(
    settings: &plugin_protocol::settings::Effective,
) -> Result<(String, String, String), Failure> {
    let runtime = select_runtime(settings)?;
    let project = project_for(settings)?;
    if runtime.java.contains("${dependency:") || project.home.contains("${dependency:") {
        return Err(Failure::new(
            ErrorCode::NotFound,
            "Debug runtimes are not prepared",
        ));
    }
    let environment = if project.major == 8 {
        "JavaSE-1.8".into()
    } else {
        format!("JavaSE-{}", project.major)
    };
    Ok((runtime.java, project.home, environment))
}

/// Match an exact project feature release and compiler before considering its private distribution.
fn select_project(major: u16) -> Result<Jdk, Failure> {
    if let Ok(project) = observe(&format!("local{major}"), None) {
        if project.major == major && has_compiler(&project) {
            return Ok(project);
        }
    }
    managed(major)
}

/// Inspect only approved local metadata; native version probes and interpreter execution are unnecessary.
fn observe(service: &str, artifact: Option<dependencies::Artifact>) -> Result<Jdk, Failure> {
    from_metadata(
        process::inspect_tool(
            process::ToolSource::Service {
                service: service.into(),
            },
            1,
            vec!["release".into()],
        )?,
        artifact,
    )
}

/// Resolve an already prepared immutable SDK, or propose its locked archive for host acquisition.
fn managed(major: u16) -> Result<Jdk, Failure> {
    let entries: Vec<Distribution> = serde_json::from_str(include_str!("../licenses/jdks.json"))
        .map_err(|_| invalid("Invalid SDK catalog"))?;
    let distribution = entries
        .into_iter()
        .find(|entry| entry.major == major)
        .ok_or_else(|| invalid("Unsupported project JDK version"))?;
    let id = format!("jdk{major}");
    let artifact = dependencies::Artifact {
        id: id.clone(),
        version: distribution.version,
        platform: "windows-x86_64".into(),
        sha256: distribution.sha256,
        source: dependencies::Source::Url {
            url: distribution.url,
        },
        format: dependencies::Format::Zip,
        requires: vec![],
        installer: None,
    };
    if let Ok(project) = observe(&format!("managed{major}"), Some(artifact.clone())) {
        if project.major != major || !has_compiler(&project) {
            return Err(invalid(
                "Prepared SDK does not match its required compiler version",
            ));
        }
        return Ok(project);
    }
    let home = format!("${{dependency:{id}}}/{}", distribution.folder);
    Ok(Jdk {
        major,
        java: format!("{home}/bin/java.exe"),
        home,
        artifact: Some(artifact),
    })
}

/// Parse standard distribution metadata in the plugin, keeping language/version interpretation out of host code.
fn from_metadata(
    metadata: process::ToolMetadata,
    artifact: Option<dependencies::Artifact>,
) -> Result<Jdk, Failure> {
    let version = metadata
        .files
        .get("release")
        .and_then(|text| {
            text.lines().find_map(|line| {
                let (key, value) = line.split_once('=')?;
                (key == "JAVA_VERSION").then_some(value.trim().trim_matches('"'))
            })
        })
        .ok_or_else(|| invalid("Tool distribution has no Java version metadata"))?;
    let number = version.strip_prefix("1.").unwrap_or(version);
    let major = number
        .split(['.', '_', '+', '-'])
        .next()
        .unwrap_or("")
        .parse()
        .map_err(|_| invalid("Invalid Java version metadata"))?;
    Ok(Jdk {
        major,
        home: metadata.root,
        java: metadata.program,
        artifact,
    })
}

/// A project SDK includes a real compiler; a language-service-only runtime may legitimately lack one.
fn has_compiler(jdk: &Jdk) -> bool {
    if jdk.home.contains("${dependency:") {
        return false;
    }
    let source = if jdk.artifact.is_some() {
        process::ToolSource::Service {
            service: format!("compiler{}", jdk.major),
        }
    } else {
        process::ToolSource::Program {
            program: super::native_path(&jdk.home, "bin/javac.exe"),
        }
    };
    process::inspect_tool(source, 0, vec![]).is_ok()
}

/// Configuration errors stay typed and do not trigger a silent fallback for explicit user selections.
fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}
