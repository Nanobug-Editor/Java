//! Build tools supply bounded plain Java projects; their formats and dependency policies stay outside Java.
use plugin_protocol::{
    api::{self, ErrorCode, Failure},
    language, language_context, language_control, service, settings,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{cell::RefCell, collections::BTreeMap};

/// Only current context descriptors and original reply handles are retained, never document text.
#[derive(Default)]
struct State {
    pending: BTreeMap<u64, (api::ResourceHandle, api::guest::EditorTask)>,
    /// Launch keys are logical transient projections; opaque registry tokens never enter saved values.
    launches: BTreeMap<String, language_context::Reference>,
}
thread_local! {static STATE:RefCell<State>=RefCell::default();}

/// Register a caller-scoped logical project, then ask the native worker to seal the original source.
/// `attach_metadata.project` accepts a plain Project or `{schema:1,project,graph}`; the graph pools
/// library paths and gives each project its own sources, library indices, references and compiler
/// release/source/target. Invalid paths, indices, sizes and levels fail before acquiring a process.
pub(crate) fn invoke(call: service::Invocation) -> Result<api::Output, Failure> {
    if matches!(call.method.as_str(), "launch" | "launch_metadata") {
        return Ok(api::Output {
            service_reply: Some(register_launch(&call)),
            ..Default::default()
        });
    }
    let reply = call
        .reply
        .clone()
        .ok_or_else(|| invalid("An asynchronous project reply is required"))?;
    let result = STATE.with_borrow_mut(|state| -> Result<(), Failure> {
        if !matches!(call.method.as_str(), "attach" | "attach_metadata") {
            return Err(invalid("Unknown Java project method"));
        }
        let document: language::DocumentContext =
            serde_json::from_value(call.arguments["document"].clone()).map_err(invalid)?;
        let metadata: Value = serde_json::from_str(
            call.arguments["project"]
                .as_str()
                .ok_or_else(|| invalid("Missing Java project"))?,
        )
        .map_err(invalid)?;
        let project: crate::project::Project = serde_json::from_value(if metadata["schema"] == 1 {
            metadata["project"].clone()
        } else {
            metadata.clone()
        })
        .map_err(invalid)?;
        project.validate().map_err(invalid)?;
        let graph = if metadata["schema"] == 1 {
            let graph: crate::project::Graph =
                serde_json::from_value(metadata["graph"].clone()).map_err(invalid)?;
            graph.validate().map_err(invalid)?;
            serde_json::to_value(graph).map_err(invalid)?
        } else {
            Value::Null
        };
        let key = format!(
            "{:x}",
            Sha256::digest(format!(
                "{}:{}",
                call.caller.instance,
                call.arguments["key"]
                    .as_str()
                    .ok_or_else(|| invalid("Missing project key"))?
            ))
        );
        let generation = call.arguments["generation"]
            .as_u64()
            .filter(|value| *value > 0)
            .ok_or_else(|| invalid("Invalid project generation"))?;
        let data = json!({"project":project,"graph":graph,"jdk":call.arguments["jdk"]});
        let descriptor = language_context::Descriptor {
            provider: "project-analysis".into(),
            key: key.clone(),
            generation,
            project_root: String::new(),
            data,
        };
        if !descriptor.valid() {
            return Err(invalid(
                "Imported Java project exceeds the native context budget",
            ));
        }
        let references = language_context::register_borrowed(vec![descriptor])?;
        let token = references
            .into_iter()
            .find(|entry| entry.descriptor.key == key)
            .ok_or_else(|| invalid("Missing registered Java context"))?
            .token;
        let task = language_context::control(
            token,
            language_control::Request {
                provider: "project-analysis".into(),
                document: language::DocumentContext {
                    generation: 0,
                    ..document.clone()
                },
                method: "java/project/ready".into(),
                arguments: Value::Null,
                context_snapshot: false,
                service_sets: vec![],
                imports: vec![],
                publish_binding: Some(document),
            },
            120_000,
        )?;
        state.pending.insert(reply.resource, (reply.clone(), task));
        Ok(())
    });
    match result {
        Ok(()) => Ok(Default::default()),
        Err(error) => Ok(api::Output {
            service_reply: Some(Err(error)),
            ..Default::default()
        }),
    }
}

/// Register only generic build outputs, leaving POM evaluation and compilation with the supplier.
fn register_launch(call: &service::Invocation) -> Result<Value, Failure> {
    let mut execution: crate::project::Execution =
        serde_json::from_str(call.arguments["project"].as_str().unwrap_or("")).map_err(invalid)?;
    if execution.jdk.is_empty() {
        execution.jdk = crate::settings::snapshot()
            .get("java.jdkVersion")
            .and_then(|setting| setting.value.as_str())
            .unwrap_or("25")
            .to_owned();
    }
    execution.validate().map_err(invalid)?;
    let main = call.arguments["main_class"].as_str().unwrap_or("");
    if !crate::project::Project::valid_launch_class(main) {
        return Err(invalid("Invalid imported main class"));
    }
    let key = format!(
        "{:x}",
        Sha256::digest(format!(
            "launch:{}:{}:{}",
            call.caller.instance,
            call.arguments["key"].as_str().unwrap_or(""),
            call.arguments["generation"]
        ))
    );
    let descriptor = language_context::Descriptor {
        provider: "project-analysis".into(),
        key: key.clone(),
        generation: call.arguments["generation"].as_u64().unwrap_or(0),
        project_root: String::new(),
        data: serde_json::to_value(&execution).map_err(invalid)?,
    };
    let mut references = language_context::register_borrowed(vec![descriptor])?;
    let reference = references
        .pop()
        .ok_or_else(|| invalid("Missing Java launch registration"))?;
    STATE.with_borrow_mut(|state| {
        state
            .launches
            .retain(|_, reference| language_context::describe(reference.token.clone()).is_ok());
        if !state.launches.contains_key(&key) && state.launches.len() >= 64 {
            return Err(invalid("Too many imported Java launch contexts"));
        }
        state.launches.insert(key.clone(), reference);
        Ok(())
    })?;
    let mut result = json!({"binding":json!({"schema":2,"key":key,"generation":call.arguments["generation"],"jdk":execution.jdk,"main_class":main}).to_string()});
    if call.method == "launch_metadata" {
        let workspace = crate::settings::workspace()?;
        let classpath = execution
            .classpath
            .iter()
            .map(|path| crate::project::rooted(&workspace, path))
            .collect::<Vec<_>>()
            .join(";");
        if classpath.len() > 32768 {
            return Err(invalid(
                "Java runtime classpath exceeds the 32 KiB environment budget",
            ));
        }
        result["env"] = if classpath.len() > 4096 {
            json!([{"name":"NANOBUG_JAVA_CLASSPATH","value":classpath}])
        } else {
            json!([])
        };
    }
    Ok(result)
}

/// An executable projection must still resolve its live owner and original discovery epoch.
pub(crate) fn launch(key: &str) -> Result<crate::project::Execution, Failure> {
    let reference = STATE
        .with_borrow(|state| state.launches.get(key).cloned())
        .ok_or_else(|| invalid("Revalidate the imported Java configuration before launch"))?;
    language_context::check(reference.token)?;
    let execution: crate::project::Execution =
        serde_json::from_value(reference.descriptor.data).map_err(invalid)?;
    execution.validate().map_err(invalid)?;
    Ok(execution)
}

/// Readonly dependency preparation uses a requested JDK hint; invocation checks the live context.
/// This cannot select an executable or grant authority, and invalid explicit JDK overrides still fail.
pub(crate) fn execution_settings(
    settings: &settings::Effective,
    arguments: &Value,
) -> Result<settings::Effective, Failure> {
    let binding: Value =
        serde_json::from_str(arguments["binding"].as_str().unwrap_or("null")).map_err(invalid)?;
    let jdk = if binding["schema"] == 2 {
        binding["jdk"].as_str()
    } else {
        arguments["args"]
            .as_array()
            .and_then(|args| {
                args.iter()
                    .position(|arg| arg == "--jdk-version")
                    .and_then(|index| args.get(index + 1))
            })
            .and_then(Value::as_str)
    };
    let mut settings = settings.clone();
    if let Some(jdk) = jdk {
        if !["8", "11", "17", "21", "25"].contains(&jdk) {
            return Err(invalid("Unsupported imported Java JDK"));
        }
        settings.insert(
            "java.jdkVersion".into(),
            settings::EffectiveValue {
                value: json!(jdk),
                source: settings::Source::Project,
            },
        );
    }
    Ok(settings)
}

/// Resolve only the matching native task, preserving transitive cancellation and typed failures.
pub(crate) fn event(event: &api::Notification) {
    STATE.with_borrow_mut(|state| {
        if let api::Notification::Service(service::Notification::InvocationCancelled {
            request,
            ..
        }) = event
        {
            if let Some((_, task)) = state.pending.remove(&request.resource) {
                let _ = task.cancel(api::CancelMode::StopWaiting);
            }
        }
        let mut completed = Vec::new();
        for (key, (_, task)) in &mut state.pending {
            let result = match task.update(event) {
                Some(api::RequestUpdate::Completed { result }) => {
                    Some(result.and_then(|value| match value {
                        api::EditorValue::LanguageBinding { binding, .. } => {
                            Ok(json!({"binding":binding}))
                        }
                        _ => Err(invalid("Expected a native Java binding")),
                    }))
                }
                Some(api::RequestUpdate::Cancelled { reason, .. }) => Some(Err(Failure::new(
                    reason,
                    "Java project preparation cancelled",
                ))),
                _ => None,
            };
            if let Some(result) = result {
                completed.push((*key, result));
            }
        }
        for (key, result) in completed {
            let (reply, _) = state.pending.remove(&key).unwrap();
            let _ = service::guest::reply(&reply, result);
        }
    });
}

/// A selected immutable profile overrides only this language plan, never persistent user settings.
pub(crate) fn context(mut context: language::Context) -> Result<language::Context, Failure> {
    if context.provider != "project-analysis" || context.preparing_dependencies {
        return Ok(context);
    }
    let selected = context
        .selected_context
        .as_ref()
        .ok_or_else(|| invalid("An imported Java context must be explicitly selected"))?;
    let project = serde_json::to_string(&selected.data["project"]).map_err(invalid)?;
    let jdk = selected.data["jdk"].as_str().unwrap_or("");
    context.settings.insert(
        "java.project".into(),
        settings::EffectiveValue {
            value: json!(project),
            source: settings::Source::Project,
        },
    );
    if !jdk.is_empty() {
        context.settings.insert(
            "java.jdkVersion".into(),
            settings::EffectiveValue {
                value: json!(jdk.strip_prefix("1.").unwrap_or(jdk)),
                source: settings::Source::Project,
            },
        );
    }
    Ok(context)
}
/// Invalid caller metadata is rejected before a process, index or binding is acquired.
fn invalid(error: impl ToString) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, error.to_string())
}
