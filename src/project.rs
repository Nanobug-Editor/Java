//! Ordinary Java source configuration contains no build-tool discovery or dependency resolver.
use serde::{Deserialize, Serialize};

/// Explicit absolute local JARs remain literal; relative project entries retain native root spelling.
pub(crate) fn rooted(workspace: &str, path: &str) -> String {
    let path = if path.starts_with(['/', '\\']) || path.as_bytes().get(1) == Some(&b':') {
        path.into()
    } else {
        if workspace.contains('\\') {
            format!(
                "{}\\{}",
                workspace.trim_end_matches('\\'),
                path.replace('/', "\\")
            )
        } else {
            format!("{}/{}", workspace.trim_end_matches('/'), path)
        }
    };
    // Older JVM class loaders interpret classpath entries as file URLs and cannot use the native
    // extended path prefix. Keep ordinary drive/UNC spelling here; host/compiler pins stay canonical.
    if let Some(tail) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{tail}")
    } else if let Some(tail) = path.strip_prefix(r"\\?\") {
        tail.into()
    } else {
        path
    }
}

/// One explicitly configured Java source project, shared by analysis and native Java execution.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Project {
    /// Workspace-relative source directories; `.` includes a standalone file's workspace.
    pub source_roots: Vec<String>,
    /// Explicit local dependency JARs; Maven and Gradle coordinates are never interpreted.
    pub jars: Vec<String>,
    /// Project-relative compiler output directory.
    pub output: String,
    /// Fully qualified launch class, configured independently from source discovery.
    pub main_class: String,
}

impl Project {
    /// Check launch-token boundaries before passing a class to the JVM. Java itself validates the
    /// complete Unicode identifier rules; paths, launcher switches and controls are never class names.
    pub fn valid_launch_class(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 4000
            && value.split('.').all(|part| {
                part.chars()
                    .next()
                    .is_some_and(|first| first.is_alphabetic() || first == '_' || first == '$')
            })
            && !value.chars().any(|character| {
                character.is_whitespace()
                    || character.is_control()
                    || matches!(character, '/' | '\\' | ':' | '"' | '\'')
            })
    }

    /// Validate portable output containment before any language or compiler process is prepared.
    /// Returns an explanation when `output` is absolute, escapes its root, or contains a stream name.
    pub fn validate(&self) -> Result<(), &'static str> {
        let output = self.output.replace('\\', "/");
        if output.is_empty()
            || output.starts_with('/')
            || output.contains(':')
            || output.chars().any(char::is_control)
            || output
                .split('/')
                .any(|part| part.is_empty() || part == ".." || part == ".")
        {
            return Err("Java output must be a non-empty workspace-relative directory");
        }
        Ok(())
    }
}

impl Default for Project {
    /// Default ordinary Java sources use the workspace root and a separate class-output directory.
    fn default() -> Self {
        Self {
            source_roots: vec![".".into()],
            jars: Vec::new(),
            output: "out".into(),
            main_class: String::new(),
        }
    }
}

/// Generic already-built project data supplied by a separate build-tool plugin.
/// Runtime entries include reactor output directories as well as resolved local JARs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Execution {
    pub project: Project,
    pub jdk: String,
    pub classpath: Vec<String>,
}
impl Execution {
    /// Validate metadata before retaining it in a live borrowed registration.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        self.project.validate()?;
        if !["8", "11", "17", "21", "25"].contains(&self.jdk.as_str())
            || self.classpath.is_empty()
            || self.classpath.len() > 128
            || self
                .classpath
                .iter()
                .any(|path| path.is_empty() || path.contains('\0') || path.len() > 4096)
        {
            return Err("Invalid imported Java execution metadata");
        }
        Ok(())
    }
}

/// Generic analysis graph retains each source project's own dependency boundary in one JVM.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Graph {
    pub libraries: Vec<String>,
    pub projects: Vec<AnalysisProject>,
}
/// References and library indices preserve the supplying build tool's effective classpath order.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnalysisProject {
    pub sources: Vec<String>,
    pub libraries: Vec<usize>,
    pub references: Vec<usize>,
    pub release: String,
    /// Optional source/target pairs remain distinct from the JDK's release API restriction.
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub target: String,
}
impl Graph {
    /// Reject malformed or excessive graphs before they acquire a native service or private index.
    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.projects.is_empty() || self.projects.len() > 128 || self.libraries.len() > 1024 {
            return Err("Invalid Java analysis graph size");
        }
        for path in &self.libraries {
            if path.is_empty() || path.len() > 4096 || path.chars().any(char::is_control) {
                return Err("Invalid Java library path");
            }
        }
        for (index, project) in self.projects.iter().enumerate() {
            if project.sources.len() > 128
                || project.libraries.len() > 1024
                || project.references.len() > 128
                || project
                    .libraries
                    .iter()
                    .any(|value| *value >= self.libraries.len())
                || project
                    .references
                    .iter()
                    .any(|value| *value >= self.projects.len() || *value == index)
                || [&project.release, &project.source, &project.target]
                    .into_iter()
                    .any(|level| {
                        !level.is_empty()
                            && level
                                .parse::<u32>()
                                .ok()
                                .is_none_or(|value| !(8..=65535).contains(&value))
                    })
            {
                return Err("Invalid Java project dependencies or compiler level");
            }
            for source in &project.sources {
                let source = source.replace('\\', "/");
                if source.len() > 4096
                    || source.starts_with('/')
                    || source.contains(':')
                    || source.chars().any(char::is_control)
                    || source.split('/').any(|part| part == "..")
                {
                    return Err("Java source escapes its workspace");
                }
            }
        }
        Ok(())
    }
}
