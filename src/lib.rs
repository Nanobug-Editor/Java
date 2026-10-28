//! Java language policy consumes public host snapshots and owns no mutable document text.
use plugin_protocol::{api, language};
use serde_json::json;
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
mod configurations;
#[cfg(target_arch = "wasm32")]
mod debugger;
#[cfg(target_arch = "wasm32")]
mod execution;
#[cfg(target_arch = "wasm32")]
mod imported;
pub mod project;
#[cfg(target_arch = "wasm32")]
mod runtime;
mod settings;

/// Prepare the host's readonly language snapshot, returning configuration or a typed invalid request.
/// `context` supplies the selected project and effective settings; this function performs no IO.
pub fn prepare_language(context: language::Context) -> Result<api::Output, api::Failure> {
    let project = settings::project(&context.settings)?;
    let settings = json!({
        "project": {
            "sourcePaths": project.source_roots,
            "referencedLibraries": project.jars,
            "outputPath": project.output
        },
        // Build-tool importers belong to their independent providers, including inside JDT LS.
        "import": {"maven": {"enabled": false}, "gradle": {"enabled": false}},
        "maven": {"downloadSources": false},
        "eclipse": {"downloadSources": false}
    });
    Ok(api::Output {
        language_service: Some(language::Proposal {
            program: Some(native_path(
                &context.package_root,
                "native/java-adapter.exe",
            )),
            args: Some(vec![
                "lsp".into(),
                "--server".into(),
                native_path(&context.package_root, "server"),
                "--data".into(),
                native_path(&context.data_root, "jdt"),
                "--java".into(),
                context
                    .settings
                    .get("java.runtime")
                    .and_then(|setting| setting.value.as_str())
                    .filter(|value| !value.is_empty())
                    .unwrap_or("java")
                    .to_owned(),
            ]),
            // A workspace-wide cold demand carries no document intent to seal a project identity.
            context: context.document.as_ref().map(|_| language::ProjectContext {
                key: "plain-java".into(),
                // Discovery intents identify individual consumers. This plain project has no
                // mutable discovery facts; the host separately keys effective settings and package.
                generation: 1,
            }),
            initialization_options: Some(json!({"settings": {"java": settings}})),
            configuration: Some(BTreeMap::from([("java".into(), settings)])),
            ..Default::default()
        }),
        ..Default::default()
    })
}

/// Append a known relative resource to a host root, retaining Windows verbatim path semantics.
/// WASM's own path implementation is POSIX; verbatim Windows roots require literal backslashes.
fn native_path(root: &str, relative: &str) -> String {
    if root.starts_with("\\\\?\\") {
        format!(
            "{}\\{}",
            root.trim_end_matches('\\'),
            relative.replace('/', "\\")
        )
    } else {
        format!("{}/{relative}", root.trim_end_matches('/'))
    }
}

/// The WASM export uses the same preparation function exercised through the public SDK types.
#[cfg(target_arch = "wasm32")]
mod guest {
    use plugin_protocol::{
        api,
        bindings::{Guest, export},
    };

    /// Java holds no secondary text buffer or persistent invocation resources.
    struct JavaPlugin;

    impl Guest for JavaPlugin {
        /// Decode a host invocation and return its typed completion; malformed messages remain errors.
        fn dispatch(payload: String) -> Result<String, String> {
            api::guest::dispatch(&payload, |input| match input {
                api::Input::Prepare { environment, .. } => {
                    super::settings::prepare(environment);
                    Ok(api::Output::default())
                }
                api::Input::Event {
                    event: api::Notification::ServicePreparation(context),
                    ..
                } => super::runtime::prepare_services(context),
                api::Input::Event {
                    event:
                        api::Notification::Configuration {
                            phase: plugin_protocol::settings::Phase::Apply,
                            values,
                        },
                    ..
                } => {
                    super::settings::apply(values);
                    Ok(api::Output::default())
                }
                api::Input::Event {
                    event:
                        api::Notification::Service(plugin_protocol::service::Notification::Invoke(call)),
                    ..
                } => {
                    if call.contract == "debug.session" {
                        super::debugger::event(api::Notification::Service(
                            plugin_protocol::service::Notification::Invoke(call),
                        ))
                    } else if call.contract == "java.project" {
                        super::imported::invoke(call)
                    } else if call.contract == plugin_protocol::targets::CONTRACT {
                        super::execution::invoke(call)
                    } else {
                        super::configurations::invoke(call)
                    }
                }
                // All durable choices use host settings. Process/session identities are never
                // restored; an explicit empty logical state still fulfills the lifecycle contract.
                api::Input::Snapshot => Ok(api::Output {
                    snapshot: Some(plugin_protocol::Snapshot {
                        schema: 1,
                        data: "{}".into(),
                    }),
                    ..Default::default()
                }),
                api::Input::Event {
                    event: api::Notification::LanguageService(context),
                    ..
                } => {
                    let context = super::imported::context(context)?;
                    let mut output = super::prepare_language(context.clone())?;
                    if let Some(selected) = &context.selected_context {
                        output.language_service.as_mut().unwrap().context =
                            Some(plugin_protocol::language::ProjectContext {
                                key: selected.key.clone(),
                                generation: selected.generation,
                            });
                        let args = output
                            .language_service
                            .as_mut()
                            .unwrap()
                            .args
                            .as_mut()
                            .unwrap();
                        args.extend(["--explicit-project".into(), "true".into()]);
                        if let Some(index) = args.iter().position(|argument| argument == "--data") {
                            args[index + 1] = super::native_path(
                                &context.data_root,
                                &format!("jdt-imported/{}-{}", selected.key, selected.generation),
                            );
                        }
                    }
                    super::runtime::prepare(&context, output.language_service.as_mut().unwrap())?;
                    if let Some(selected) = &context.selected_context {
                        if !selected.data["graph"].is_null() {
                            output
                                .language_service
                                .as_mut()
                                .unwrap()
                                .initialization_options
                                .as_mut()
                                .unwrap()["nanobugProjects"] = selected.data["graph"].clone();
                        }
                    }
                    Ok(output)
                }
                api::Input::Event { event, .. } => {
                    super::imported::event(&event);
                    let output = super::execution::event(&event)?;
                    super::debugger::event(event)?;
                    Ok(output)
                }
                _ => Ok(api::Output::default()),
            })
        }
    }
    export!(JavaPlugin);
}
