# Java for Nanobug

Version **0.1.35** targets the Nanobug internal-test build of 2026-11-02 through the public host SDK. Installation IDs and permissions are preserved. Windows native helpers included in this package use a statically linked runtime.

Independent Java language plugin, currently under development. Maven is implemented separately;
Gradle, Lombok and JUnit remain separate planned plugins.

[简体中文](README.zh-CN.md)

The development version provides plain Java project settings, controlled JDT LS analysis, and private JDK preparation. Java 8, 11, 17, 21 and 25 selection and semantic diagnostics have been exercised through actual packages. Native preview and group Undo cover cross-file symbol rename, including top-level class filenames; quick fixes and import organization also use preview. JDK and local JAR code opens read-only, using local sources before decompilation. Native GPUI tests have passed compilation, console input/output, Stop, Rerun and complete debug interaction, including thread selection, all three step controls, lazy objects and array/collection paging. Package/directory refactors and resource creation/deletion are not supported by the current editing bridge.

The analysis, compile/run and launch-debug matrix has passed on all five JDK versions, including an independent target working directory. Tree-sitter 0.23.5 parses Java 25 compact sources but currently rejects `import module`; its patch still needs a verified grammar build. Older packages show an update requirement. Use the separately versioned Release package with the matching Nanobug host.

Build a development package through the selected Nanobug executable:

```powershell
Nanobug.exe --plugin-package C:/Projects/RustProjects/Java-for-Nanobug
```

`java.project` contains a JSON object with `source_roots`, `jars`, `output`, and `main_class`. Source roots default to `["."]` and output to `out`. JDT's Maven and Gradle importers stay disabled; independent plugins own build-tool import and dependency resolution.

The public `java.project` 1.2 service accepts bounded generic analysis and already-built launch
metadata from build providers. Analysis graphs retain separate source projects, exact libraries,
project references and compiler levels in one shared session, so unsaved changes and refactoring
reach dependent modules. Eclipse metadata and analysis outputs stay in Java's private index.
Borrowed contexts follow the supplying instances and their discovery epochs; refresh or retirement
withdraws old services. Launch metadata uses the supplied runtime classpath without invoking javac.
These methods require host `language.lsp` 1.9. Java never evaluates a POM or a build script.

Create a **Java application** in Run configurations and enter its main class, literal arguments, working directory and environment. Build compiles all configured sources and never executes main. Run starts the compiled target once; failed compilation blocks launch. Open a workspace source file and use **F9** to toggle its cursor line as a breakpoint for the selected configuration, then Debug. The native task panel offers thread selection, stack frames and variable expansion; **Load more** requests another bounded page. Debug backend indices remain in private storage. Source breakpoint shortcuts use the ordinary remappable host keymap.

`java.jdkVersion` selects 8, 11, 17, 21 or 25 (default 25). Compatible local tools are reused; otherwise the host prepares the pinned distribution in its private cache. `java.jdkHome` overrides the project JDK with a local directory; an explicitly chosen version must match it. `java.runtime` independently selects the language-server Java executable (21 or newer). Invalid explicit tools fail visibly. No global environment is changed. Configuration is stored by Nanobug for the workspace on this machine.

The development server requires Java 21 or newer. It communicates over standard input/output. Native programs run with the current OS user's authority. Mutable server configuration and indexes stay in the host-managed private data directory.

A native process mutex protects each index. Eclipse byte-range locks are disabled so the host can prepare an update copy while the old instance remains usable. Cutover still terminates the old language/debug processes and never replays an old target. See the [Eclipse runtime options](https://help.eclipse.org/latest/topic/org.eclipse.platform.doc.isv/reference/misc/runtime-options.html).

Third-party distribution notices are included in `server` and `licenses`. Server and grammar source identities are recorded in `licenses/sources.json`; managed Windows x64 JDK versions, immutable download URLs and checksums are recorded in `licenses/jdks.json`. Other platforms have not been verified.
