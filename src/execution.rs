//! Java targets compile in their original preparation session, then return a literal native launch.
use crate::project::rooted;
use plugin_protocol::{
    api::{self, ErrorCode, Failure},
    process, service,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{cell::RefCell, collections::BTreeMap};

thread_local! {
    /// Live preparation bookkeeping contains handles and immutable results, never source text.
    static BUILDS: RefCell<Vec<Build>> = const { RefCell::new(Vec::new()) };
}
/// Each child retains its original deferred reply; parallel configurations cannot exchange results.
struct Build {
    process: api::ResourceHandle,
    reply: api::ResourceHandle,
    prepared: Value,
}

/// Bindings are portable plugin data. A caller cannot inject a tool path, SDK source or raw compiler args.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Binding {
    schema: u32,
    #[serde(default)]
    main_class: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    generation: u64,
    #[serde(default)]
    jdk: String,
}
/// Use only this invocation's effective project policy and declared approved tools, preserving errors.
/// Dependency preparation is method metadata, not a download or arbitrary executable request here.
pub(crate) fn invoke(call: service::Invocation) -> Result<api::Output, Failure> {
    if call.method == "prepare" {
        return Ok(match prepare(&call) {
            Ok(()) => api::Output::default(),
            Err(error) => api::Output {
                service_reply: Some(Err(error)),
                ..Default::default()
            },
        });
    }
    let result = (|| {
        let settings = crate::settings::snapshot();
        let project = crate::settings::project(&settings)?;
        match call.method.as_str() {
            "discover" => {
                if project.main_class.is_empty() {
                    return Ok(json!({"targets":[]}));
                }
                if !crate::project::Project::valid_launch_class(&project.main_class) {
                    return Err(invalid("Invalid configured Java main class"));
                }
                Ok(json!({"targets":[{
                    "identity":"application", "label":project.main_class.chars().take(80).collect::<String>(),
                    "source":"", "target_type":"java-application", "type_version":1,
                    "binding":json!({"schema":1,"main_class":project.main_class}).to_string()
                }]}))
            }
            _ => Err(Failure::new(
                ErrorCode::UnsupportedOperation,
                "Unsupported Java target method",
            )),
        }
    })();
    Ok(api::Output {
        service_reply: Some(result),
        ..Default::default()
    })
}

/// Start one ordinary bounded compiler in the invocation's existing preparation resource root.
/// The eventual executable is returned only after compiler exit; host cancellation owns its tree.
fn prepare(call: &service::Invocation) -> Result<(), Failure> {
    let binding: Binding = serde_json::from_str(call.arguments["binding"].as_str().unwrap_or(""))
        .map_err(|error| invalid(&error.to_string()))?;
    if binding.schema == 2 {
        let execution = crate::imported::launch(&binding.key)?;
        if !crate::project::Project::valid_launch_class(&binding.main_class)
            || execution.jdk != binding.jdk
            || binding.generation == 0
        {
            return Err(invalid("Invalid imported Java launch binding"));
        }
        let settings =
            crate::imported::execution_settings(&crate::settings::snapshot(), &call.arguments)?;
        let (_, java) = crate::runtime::prepared_project(&settings)?;
        let adapter = process::inspect_tool(
            process::ToolSource::Service {
                service: "analysis".into(),
            },
            0,
            vec![],
        )?;
        let workspace = call.arguments["workspace"]
            .as_str()
            .ok_or_else(|| invalid("Missing imported workspace"))?;
        let classpath = execution
            .classpath
            .iter()
            .map(|path| rooted(workspace, path))
            .collect::<Vec<_>>()
            .join(";");
        let classpath = if classpath.len() > 4096 {
            if !call.arguments["env"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|entry| {
                    entry["name"] == "NANOBUG_JAVA_CLASSPATH"
                        && entry["value"].as_str() == Some(classpath.as_str())
                })
            {
                return Err(invalid(
                    "Revalidate the Java configuration's runtime environment",
                ));
            }
            "${nanobug:classpath}".into()
        } else {
            classpath
        };
        let prepared = json!({"program":adapter.program,"prefix_args":["run","--java",java,"--context",binding.key,"--jdk-version",binding.jdk,"--","-Dfile.encoding=UTF-8","-Dsun.stdout.encoding=UTF-8","-Dsun.stderr.encoding=UTF-8","-classpath",classpath,binding.main_class]});
        service::guest::reply(
            call.reply
                .as_ref()
                .ok_or_else(|| invalid("Missing preparation reply"))?,
            Ok(prepared),
        )?;
        return Ok(());
    }
    if BUILDS.with_borrow(|builds| builds.len() >= 8) {
        return Err(Failure::new(
            ErrorCode::LimitExceeded,
            "Too many Java builds",
        ));
    }
    let reply = call
        .reply
        .clone()
        .ok_or_else(|| invalid("Missing Java preparation reply"))?;
    let settings = crate::settings::snapshot();
    let project = crate::settings::project(&settings)?;
    let workspace = call.arguments["workspace"]
        .as_str()
        .ok_or_else(|| invalid("Missing Java workspace"))?;
    let binding: Binding = serde_json::from_str(call.arguments["binding"].as_str().unwrap_or(""))
        .map_err(|error| invalid(&error.to_string()))?;
    if binding.schema != 1 || !crate::project::Project::valid_launch_class(&binding.main_class) {
        return Err(invalid("Invalid Java application binding"));
    }
    let (jdk, java) = crate::runtime::prepared_project(&settings)?;
    let adapter = process::inspect_tool(
        process::ToolSource::Service {
            service: "analysis".into(),
        },
        0,
        Vec::new(),
    )?;
    let mut classpath = vec![rooted(workspace, &project.output)];
    classpath.extend(project.jars.iter().map(|jar| rooted(workspace, jar)));
    let separator = if workspace.as_bytes().get(1) == Some(&b':') || workspace.starts_with("\\\\") {
        ";"
    } else {
        ":"
    };
    // The owned launcher sets this execution's console encoding before Java initializes stdio.
    let prepared = json!({"program":adapter.program,"prefix_args":[
        "run","--java",java,"--",
        "-Dfile.encoding=UTF-8","-Dsun.stdout.encoding=UTF-8","-Dsun.stderr.encoding=UTF-8",
        "-classpath",classpath.join(separator),binding.main_class
    ]});
    let mut env = BTreeMap::new();
    for entry in call.arguments["env"].as_array().into_iter().flatten() {
        let name = entry["name"]
            .as_str()
            .ok_or_else(|| invalid("Invalid Java build environment"))?;
        let value = entry["value"]
            .as_str()
            .ok_or_else(|| invalid("Invalid Java build environment"))?;
        env.insert(name.into(), value.into());
    }
    let process = api::guest::request(api::Operation::Process {
        operation: process::Operation::Execute {
            program: adapter.program,
            args: vec![
                "compile".into(),
                "--jdk".into(),
                jdk,
                "--workspace".into(),
                workspace.into(),
                "--project".into(),
                serde_json::to_string(&project).map_err(|error| invalid(&error.to_string()))?,
            ],
            transport: process::Transport::Stdio,
            cwd: Some(workspace.into()),
            env,
        },
    })?;
    let api::Value::Resource(process) = process else {
        return Err(invalid("Missing Java compiler resource"));
    };
    BUILDS.with_borrow_mut(|builds| {
        builds.push(Build {
            process,
            reply,
            prepared,
        })
    });
    Ok(())
}

/// Ordered native process completion settles only its own deferred reply after all compiler output.
/// Source cancellation removes bookkeeping and closes any still-owned handle without replaying work.
pub(crate) fn event(event: &api::Notification) -> Result<api::Output, Failure> {
    let selected = BUILDS.with_borrow_mut(|builds| {
        let index = builds.iter().position(|build| match event {
            api::Notification::Process {
                handle,
                update: process::Update::Exited { .. } | process::Update::Terminated,
            } => handle == &build.process,
            api::Notification::Service(service::Notification::InvocationCancelled {
                request,
                ..
            }) => request == &build.reply,
            _ => false,
        })?;
        Some(builds.swap_remove(index))
    });
    if let Some(build) = selected {
        let _ = api::guest::close_resource(build.process);
        if let api::Notification::Process { update, .. } = event {
            let result = match update {
                process::Update::Exited { code: 0 } => Ok(build.prepared),
                process::Update::Exited { code } => Err(Failure::new(
                    ErrorCode::OperationFailed,
                    format!("Java compilation failed (exit {code}); see build output"),
                )),
                _ => Err(Failure::new(
                    ErrorCode::Cancelled,
                    "Java compilation was stopped",
                )),
            };
            // A retired source can reject its late receipt; it never becomes another launch's result.
            let _ = service::guest::reply(&build.reply, result);
        }
    }
    Ok(api::Output::default())
}

/// Invalid bindings/policy remain typed failures without native process side effects.
fn invalid(message: &str) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, message)
}
