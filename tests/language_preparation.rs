//! Public SDK preparation scenarios keep ordinary Java projects independent of build-tool plugins.
use nanobug_java_guest::prepare_language;
use plugin_protocol::language;
use serde_json::json;

/// Preparing an explicitly configured source folder preserves its classpath and disables importers.
#[test]
fn ordinary_source_project_prepares_language_without_build_tool_importers() {
    let context: language::Context = serde_json::from_value(json!({
        "provider": "java/analysis",
        "workspace": "C:/fixtures/plain-java",
        "package_root": "C:/fixtures/java-package",
        "data_root": "C:/fixtures/java-data",
        "document": {"path": "src/example/Main.java", "generation": 4},
        "settings": {
            "java.project": {
                "source": "project",
                "value": "{\"source_roots\":[\"src\",\"shared\"],\"jars\":[\"lib/example.jar\"],\"output\":\"out\",\"main_class\":\"example.Main\"}"
            }
        },
        "candidates": {}
    })).unwrap();
    let output = prepare_language(context).expect("prepare an ordinary Java source project");
    let plan = output.language_service.expect("language plan");
    let settings = &plan.configuration.unwrap()["java"];
    assert_eq!(settings["project"]["sourcePaths"], json!(["src", "shared"]));
    assert_eq!(
        settings["project"]["referencedLibraries"],
        json!(["lib/example.jar"])
    );
    assert_eq!(settings["project"]["outputPath"], "out");
    assert_eq!(settings["import"]["maven"]["enabled"], false);
    assert_eq!(settings["import"]["gradle"]["enabled"], false);
    assert!(
        plan.context.is_some(),
        "document-scoped preparation publishes its project context"
    );
}

/// A source configuration cannot redirect compiler outputs outside its owning workspace.
#[test]
fn project_preparation_rejects_an_escaping_output_before_starting_a_service() {
    let context: language::Context = serde_json::from_value(json!({
        "provider": "java/analysis", "workspace": "C:/fixtures/plain-java",
        "data_root": "C:/fixtures/private", "candidates": {},
        "settings": {"java.project": {"source": "project", "value": "{\"output\":\"../another-project\"}"}}
    })).unwrap();
    let error = prepare_language(context).expect_err("an escaping output must fail preparation");
    assert_eq!(error.code, plugin_protocol::api::ErrorCode::InvalidRequest);
}

/// Different document discovery intents in one ordinary project must reuse its analysis context.
#[test]
fn ordinary_sources_share_a_project_context_across_document_intents() {
    let context: language::Context = serde_json::from_value(json!({
        "provider":"java/analysis", "workspace":"C:/fixtures/plain-java",
        "document":{"path":"src/First.java","generation":4},
        "settings":{}, "candidates":{}
    }))
    .unwrap();
    let mut second = context.clone();
    second.document = Some(language::DocumentContext {
        path: "src/Second.java".into(),
        generation: 9,
    });
    let first = prepare_language(context)
        .unwrap()
        .language_service
        .unwrap()
        .context
        .unwrap();
    let second = prepare_language(second)
        .unwrap()
        .language_service
        .unwrap()
        .context
        .unwrap();
    assert_eq!(
        first, second,
        "document discovery generations do not identify different Java projects"
    );
}
