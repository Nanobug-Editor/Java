# Java for Nanobug

版本 **0.1.35** 面向 2026-11-02 的 Nanobug 内部测试版，使用公开宿主 SDK 构建。保留安装 ID 和权限；包内附带的 Windows 原生辅助程序使用静态运行库。

独立 Java 语言插件，正在开发。Maven 已独立实现；Gradle、Lombok、JUnit 仍为后续独立插件。

[English](README.md)

当前开发版包含普通 Java 源码项目配置、受控 JDT LS 分析与项目 JDK 自动准备。Java 8、11、17、21、25 的选择和语义诊断已通过真实安装包验证。跨文件符号重命名（含顶层类文件名）使用原生预览与整组撤销，快速修复和整理 import 也先预览。JDK 与本地 JAR 代码以只读文档打开，优先使用本地源码，缺少时反编译。原生编译、控制台输入输出、停止、重跑及完整调试交互已通过原生 GPUI 测试，包含线程选择、三种单步、对象按需展开和数组／集合分页。当前编辑桥不支持包／目录级重构及资源创建、删除。

五代 JDK 的分析、编译运行及启动调试矩阵、独立运行目录已通过。当前 Tree-sitter 0.23.5 可解析 Java 25 compact source，尚不能解析 `import module`；语法补丁待构建验证。旧版包会给出更新提示，请使用独立版本的 Release 包，并搭配匹配的 Nanobug 宿主。

通过选定的 Nanobug 主程序构建开发包：

```powershell
Nanobug.exe --plugin-package C:/Projects/RustProjects/Java-for-Nanobug
```

`java.project` 使用 JSON 对象配置 `source_roots`（源码目录）、`jars`（本地依赖）、`output`（编译输出）和 `main_class`（主类）。源码目录默认为 `["."]`，输出为 `out`。JDT 的 Maven／Gradle 导入器保持关闭，构建工具工程导入与依赖解析由独立插件提供。

公开的 `java.project` 1.2 服务可接收构建提供者传入的有界通用分析图和已构建启动元数据。
分析图在同一会话内保留各源码工程的独立类路径、工程引用和编译级别，使未保存修改和重构
可以覆盖依赖模块；Eclipse 元数据及分析输出保存在 Java 私有索引。借用上下文跟随提供者
实例及其发现代次，刷新或退休后撤销。启动元数据直接使用提供的运行类路径，不调用 javac。
这些方法要求宿主 `language.lsp` 1.9；Java 不读取 POM 或解释构建脚本。

在运行配置中新建 **Java 应用**，填写主类、字面参数、工作目录和环境。构建会编译全部配置源码，不执行主类；运行只启动一次目标，编译失败会阻断启动。打开工作区源码，以 **F9** 切换光标所在行的断点，再点击调试。原生任务面板提供线程选择、调用栈与变量展开，**加载更多** 显式读取下一页。调试索引保存在私有目录；F9 可通过宿主普通快捷键设置修改。

`java.jdkVersion` 可选 8、11、17、21、25，默认为 25。优先复用匹配的本机工具，缺少时由宿主准备到私有缓存。`java.jdkHome` 可覆盖为本机 JDK 目录；若同时显式选择版本，两者必须匹配。`java.runtime` 独立指定语言服务的 Java 可执行文件，要求至少 Java 21。无效的显式工具会明确报错。不修改全局环境；配置由 Nanobug 按本机工作区保存。

当前开发语言服务要求 Java 21 或更新版本，经标准输入输出通信。原生程序具有当前操作系统用户的权限。服务可变配置和索引位于宿主管理的私有目录。

每个索引由原生进程互斥保护；Eclipse 的字节范围锁关闭，以便宿主在旧实例可用时准备更新副本。正式切换仍会终止旧语言和调试进程，不恢复旧目标。配置依据见 [Eclipse 运行参数](https://help.eclipse.org/latest/topic/org.eclipse.platform.doc.isv/reference/misc/runtime-options.html)。

第三方发行许可保留在 `server` 和 `licenses`；服务与 grammar 的来源身份记录在 `licenses/sources.json`。受管 Windows x64 JDK 的版本、固定下载地址和摘要记录在 `licenses/jdks.json`，其他平台尚未验收。
