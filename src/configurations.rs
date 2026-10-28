//! Java owns application values; the public native UI and command-fields SDK own generic controls.
use plugin_protocol::{
    api::{self, ErrorCode, Failure},
    configurations::{self as contract, command_form::Fields},
    service, ui,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Return the canonical configuration envelope through the original result-bearing service call.
/// Pure catalogs/forms neither prepare a JDK nor launch a process; invalid values stay typed errors.
pub(crate) fn invoke(call: service::Invocation) -> Result<api::Output, Failure> {
    let result = (|| {
        if call.contract != contract::CONTRACT {
            return Err(Failure::new(
                ErrorCode::UnsupportedOperation,
                "Unsupported Java service",
            ));
        }
        let project = crate::settings::project(&crate::settings::snapshot())?;
        let locale = call.arguments["locale"].as_str().unwrap_or("zh-CN");
        let defaults = Values::new(project.main_class, locale);
        match call.method.as_str() {
            "catalog" => envelope(&contract::Catalog {
                templates: vec![contract::Template {
                    id: "application".into(),
                    group: "Java".into(),
                    label: label(locale, "Java 应用程序", "Java Application"),
                    icon: "code".into(),
                    defaults: serde_json::to_string(&defaults).map_err(invalid)?,
                    unavailable: None,
                }],
            }),
            "form" => {
                if call.arguments["template"] != "application" {
                    return Err(Failure::new(
                        ErrorCode::InvalidRequest,
                        "Unknown Java application template",
                    ));
                }
                let text = call.arguments["values"].as_str().unwrap_or("");
                let mut values = if text.is_empty() || text == "{}" {
                    defaults
                } else {
                    serde_json::from_str::<Values>(text).map_err(invalid)?
                };
                values.edit(call.arguments["event"].as_str().unwrap_or(""))?;
                envelope(&values.form(locale)?)
            }
            "validate" => {
                if call.arguments["template"] != "application" {
                    return Err(Failure::new(
                        ErrorCode::InvalidRequest,
                        "Unknown Java application template",
                    ));
                }
                let values: Values =
                    serde_json::from_str(call.arguments["values"].as_str().unwrap_or(""))
                        .map_err(invalid)?;
                envelope(
                    &values.validation(locale, call.arguments["workspace"].as_str().unwrap_or("")),
                )
            }
            _ => Err(Failure::new(
                ErrorCode::UnsupportedOperation,
                "Unsupported Java configuration method",
            )),
        }
    })();
    Ok(api::Output {
        service_reply: Some(result),
        ..Default::default()
    })
}

/// All editable values round-trip through the host's configuration store; guest memory owns no drafts.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Values {
    main_class: String,
    command: Fields,
    revision: u64,
}
impl Values {
    /// Project defaults seed a new portable configuration; subsequent user edits remain authoritative.
    fn new(main_class: String, locale: &str) -> Self {
        Self {
            main_class,
            command: Fields::new(
                label(locale, "Java 应用程序", "Java Application"),
                Vec::new(),
            ),
            revision: 1,
        }
    }

    /// Main-class input remains Java policy; ordinary name/argv/env edits keep SDK composition behavior.
    fn edit(&mut self, event: &str) -> Result<(), Failure> {
        if event.is_empty() {
            return Ok(());
        }
        let decoded: contract::FormEvent = serde_json::from_str(event).map_err(invalid)?;
        match decoded {
            contract::FormEvent::Native(ui::UiEvent {
                node,
                action: ui::Action::Change(value) | ui::Action::Submit(value),
                ..
            }) if node == "java-main-class" => self.main_class = value,
            _ => self.command.edit(event)?,
        }
        self.revision = self.revision.checked_add(1).ok_or_else(|| {
            Failure::new(ErrorCode::LimitExceeded, "Java form revision exhausted")
        })?;
        Ok(())
    }

    /// The host mounts these actual native controls and supplies theme, focus, IME and Save actions.
    fn form(&self, locale: &str) -> Result<contract::Form, Failure> {
        let mut document = self.command.document("java", locale, self.revision);
        document.root = ui::Node::column(
            "java-form",
            vec![
                ui::Node::text("java-main-class-label", label(locale, "主类", "Main class")),
                ui::Node::input(
                    "java-main-class",
                    ui::Input {
                        value: self.main_class.clone(),
                        value_revision: self.command.input_revision,
                        placeholder: "com.example.Main".into(),
                    },
                ),
                document.root,
            ],
        )
        .gap(6.)
        .grow();
        Ok(contract::Form {
            values: serde_json::to_string(self).map_err(invalid)?,
            name: self.command.name.clone(),
            program: "java".into(),
            document,
        })
    }

    /// Saving/launching validates owned values and returns portable provider data, never a resolved
    /// machine executable or extra grants. The later target preparation owns JDK/tool selection.
    fn validation(&self, locale: &str, workspace: &str) -> contract::Validation {
        let main = &self.main_class;
        let main_problem = !crate::project::Project::valid_launch_class(main);
        if let Some(message) = self.command.problem(locale).or_else(|| {
            main_problem.then(|| {
                label(
                    locale,
                    "请输入有效的主类名",
                    "Enter a valid main class name",
                )
            })
        }) {
            return contract::Validation {
                valid: false,
                message,
                launch: None,
            };
        }
        let mut launch = self.command.launch("java", workspace);
        launch.target = json!({
            "mode":"provided", "provider":"java", "label":self.command.name,
            "binding":json!({"schema":1,"main_class":main}).to_string(),
            "args":self.command.args,
        });
        // Build is one ordinary provider action. Run and Debug share this frozen sequence;
        // Build alone executes this action without starting the application's main class.
        launch.build = vec![json!({
            "name":label(locale,"编译 Java","Compile Java"),
            "target":{"kind":"action","target":{
                "mode":"provided","provider":"java","label":"Java compiler",
                "binding":json!({"schema":1,"main_class":main}).to_string(),"args":[]
            }}
        })];
        contract::Validation {
            valid: true,
            message: String::new(),
            launch: Some(launch),
        }
    }
}

/// Envelopes use the public bounded payload shape; the broker rechecks size and native admission.
fn envelope(value: &impl Serialize) -> Result<Value, Failure> {
    Ok(json!({"payload":serde_json::to_string(value).map_err(invalid)?}))
}
/// Configuration parse/serialization errors do not change any host draft or execution state.
fn invalid(error: impl std::fmt::Display) -> Failure {
    Failure::new(ErrorCode::InvalidRequest, error.to_string())
}
/// Provider labels follow the supplied locale; user-entered values are never translated.
fn label(locale: &str, zh: &str, en: &str) -> String {
    if locale.starts_with("zh") {
        zh.into()
    } else {
        en.into()
    }
}
