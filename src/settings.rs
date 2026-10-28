//! Host-owned setting snapshots feed the shared project model without coupling native compilation to SDK IO.
use crate::project::Project;
use plugin_protocol::{
    api::{ErrorCode, Failure},
    settings::Effective,
};

#[cfg(target_arch = "wasm32")]
thread_local! {
    /// Initial activation and rollback reapply these immutable values; guest snapshots never persist them.
    static EFFECTIVE: std::cell::RefCell<Effective> = std::cell::RefCell::default();
    /// Original workspace identity comes from Prepare, independently of any target's working directory.
    static WORKSPACE: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

#[cfg(target_arch = "wasm32")]
/// Retain the host's original workspace root for this instance; project settings cannot replace it.
pub(crate) fn prepare(environment: plugin_protocol::Environment) {
    WORKSPACE.with_borrow_mut(|workspace| *workspace = environment.workspace);
}

#[cfg(target_arch = "wasm32")]
/// Return this prepared instance's source root, failing rather than borrowing a target cwd or scope key.
pub(crate) fn workspace() -> Result<String, Failure> {
    WORKSPACE.with_borrow(|workspace| {
        if workspace.is_empty() {
            Err(Failure::new(
                ErrorCode::InvalidState,
                "Java has no prepared workspace",
            ))
        } else {
            Ok(workspace.clone())
        }
    })
}

#[cfg(target_arch = "wasm32")]
/// Apply only a host-validated setting notification before any business service consumes it.
pub(crate) fn apply(values: Effective) {
    EFFECTIVE.with_borrow_mut(|settings| *settings = values);
}

#[cfg(target_arch = "wasm32")]
/// Read one immutable effective map for this invocation, independently from language-server startup.
pub(crate) fn snapshot() -> Effective {
    EFFECTIVE.with_borrow(Clone::clone)
}

/// Decode this plugin's project setting, using defaults only when absent. Invalid JSON, shape or
/// output containment returns InvalidRequest before analysis, forms or compiler preparation.
pub(crate) fn project(settings: &Effective) -> Result<Project, Failure> {
    let project = match settings.get("java.project") {
        Some(setting) => {
            serde_json::from_str::<Project>(setting.value.as_str().ok_or_else(|| {
                Failure::new(
                    ErrorCode::InvalidRequest,
                    "java.project must contain JSON text",
                )
            })?)
            .map_err(|error| Failure::new(ErrorCode::InvalidRequest, error.to_string()))?
        }
        None => Project::default(),
    };
    project
        .validate()
        .map_err(|message| Failure::new(ErrorCode::InvalidRequest, message))?;
    Ok(project)
}
