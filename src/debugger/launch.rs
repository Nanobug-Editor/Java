//! Decode only Java's prepared literal target; runtime selection and DAP launch remain plugin policy.
use super::transport::failure;
use plugin_protocol::{
    api::{self, Failure, ResourceHandle},
    process, service,
};
use serde_json::{Value, json};

/// Start the private bridge and return its immutable launch state. Compilation has already finished.
/// Reject another provider's command shape rather than guessing at arbitrary JVM launcher options.
pub(super) fn start(
    id: &str,
    call: &service::Invocation,
) -> Result<(ResourceHandle, Value), Failure> {
    let mut args = call.arguments["args"]
        .as_array()
        .ok_or_else(|| failure("Missing Java debug arguments"))?
        .iter()
        .map(|arg| {
            arg.as_str()
                .map(str::to_owned)
                .ok_or_else(|| failure("Invalid Java debug argument"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let imported = if args.get(3).is_some_and(|arg| arg == "--context") {
        if args.len() < 8 || args[5] != "--jdk-version" || args[7] != "--" {
            return Err(failure("Invalid imported Java debug context"));
        }
        let execution = crate::imported::launch(&args[4])?;
        if execution.jdk != args[6] {
            return Err(failure("Imported Java debug JDK changed"));
        }
        args.drain(3..7);
        Some(execution)
    } else {
        None
    };
    if args.len() < 10
        || args[0] != "run"
        || args[1] != "--java"
        || args[3] != "--"
        || args[7] != "-classpath"
    {
        return Err(failure("Debug requires a prepared Java application target"));
    }
    if !crate::project::Project::valid_launch_class(&args[9]) {
        return Err(failure("Invalid Java debug main class"));
    }
    let adapter = process::inspect_tool(
        process::ToolSource::Service {
            service: "analysis".into(),
        },
        0,
        vec![],
    )?;
    if call.arguments["program"].as_str() != Some(adapter.program.as_str()) {
        return Err(failure("Debug target belongs to another provider"));
    }
    let settings =
        crate::imported::execution_settings(&crate::settings::snapshot(), &call.arguments)?;
    let explicit = imported.is_some();
    let workspace = crate::settings::workspace()?;
    let classpath = imported
        .as_ref()
        .map(|execution| {
            execution
                .classpath
                .iter()
                .map(|path| crate::project::rooted(&workspace, path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| args[8].split(';').map(str::to_owned).collect());
    let project = match imported {
        Some(mut execution) => {
            execution.project.jars = execution
                .classpath
                .iter()
                .filter(|path| path.to_ascii_lowercase().ends_with(".jar"))
                .cloned()
                .collect();
            execution.project
        }
        None => crate::settings::project(&settings)?,
    };
    let (runtime, home, environment) = crate::runtime::debug_tools(&settings)?;
    let context = process::native_context("analysis")?;
    let cwd = call.arguments["cwd"]
        .as_str()
        .filter(|value| !value.is_empty())
        .unwrap_or(&workspace)
        .to_owned();
    let data = crate::native_path(&context.data, &format!("debug-session-{id}"));
    let encoded_project =
        serde_json::to_string(&project).map_err(|error| failure(error.to_string()))?;
    let mut launch_env = std::collections::BTreeMap::new();
    let project_argument = if encoded_project.len() > 4096 {
        launch_env.insert("NANOBUG_JAVA_PROJECT".into(), encoded_project);
        "${nanobug:project}".into()
    } else {
        encoded_project
    };
    let mut options = vec![
        "debug".into(),
        "--server".into(),
        crate::native_path(&context.assets, "server"),
        "--data".into(),
        data,
        "--java".into(),
        runtime,
        "--project-jdk".into(),
        home,
        "--execution-environment".into(),
        environment,
        "--workspace".into(),
        workspace.clone(),
        "--resources".into(),
        context.assets,
        "--project".into(),
        project_argument,
    ];
    if explicit {
        options.extend(["--explicit-project".into(), "true".into()]);
    }
    let api::Value::Resource(handle) = api::guest::request(api::Operation::Process {
        operation: process::Operation::Execute {
            program: adapter.program.clone(),
            args: options,
            transport: process::Transport::Stdio,
            cwd: Some(workspace.clone()),
            env: launch_env,
        },
    })?
    else {
        return Err(failure("Expected Java debug bridge resource"));
    };
    let env: serde_json::Map<String, Value> = call.arguments["env"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| Some((entry["name"].as_str()?.into(), entry["value"].clone())))
        .collect();
    let sources: Vec<_> = project
        .source_roots
        .iter()
        .map(|source| {
            if source == "." {
                workspace.clone()
            } else {
                crate::native_path(&workspace, source)
            }
        })
        .collect();
    let dap = json!({"mainClass":args[9],"javaExec":args[2],"classPaths":classpath,"modulePaths":[],
        "args":arguments(&args[10..]),"vmArgs":arguments(&args[4..7]),"cwd":cwd,"env":env,"console":"integratedTerminal","encoding":"UTF-8",
        "sourcePaths":sources,"stopOnEntry":call.arguments["stop_on_entry"].as_bool().unwrap_or(false),"shortenCommandLine":"jarmanifest"});
    let mut launch = call.arguments.clone();
    launch["dap"] = dap;
    Ok((handle, launch))
}

/// Java Debug Server decodes Windows command-line strings before producing a literal argv array.
/// Quote every token, doubling backslashes only before quotes/end; no text is evaluated by a shell.
fn arguments(args: &[String]) -> String {
    args.iter()
        .map(|arg| {
            let mut result = String::from("\"");
            let mut slashes = 0;
            for character in arg.chars() {
                if character == '\\' {
                    slashes += 1;
                    continue;
                }
                if character == '"' {
                    result.push_str(&"\\".repeat(slashes * 2 + 1));
                } else {
                    result.push_str(&"\\".repeat(slashes));
                }
                slashes = 0;
                result.push(character);
            }
            result.push_str(&"\\".repeat(slashes * 2));
            result.push('"');
            result
        })
        .collect::<Vec<_>>()
        .join(" ")
}
