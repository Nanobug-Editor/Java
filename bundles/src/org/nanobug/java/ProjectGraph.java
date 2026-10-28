package org.nanobug.java;

import com.google.gson.JsonArray;
import com.google.gson.JsonObject;
import com.google.gson.JsonParser;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashMap;
import java.util.List;
import org.eclipse.core.resources.IFolder;
import org.eclipse.core.resources.IProject;
import org.eclipse.core.resources.IResource;
import org.eclipse.core.resources.ResourcesPlugin;
import org.eclipse.core.runtime.IPath;
import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.jdt.core.IClasspathEntry;
import org.eclipse.jdt.core.IJavaProject;
import org.eclipse.jdt.core.JavaCore;
import org.eclipse.jdt.ls.core.internal.ProjectUtils;
import org.eclipse.jdt.ls.core.internal.managers.ProjectsManager;
import org.eclipse.jdt.ls.core.internal.managers.UnmanagedFolderNature;

/** Generic project references preserve separate classpaths and shared unsaved working copies. */
public final class ProjectGraph {
    /** One graph is immutable for this private JVM/index incarnation. */
    private static boolean prepared;

    /** Apply only the owned adapter's bounded graph file; no project format or build tool is read. */
    public static synchronized void prepare(IPath root, IProgressMonitor monitor) throws Exception {
        if (prepared) return;
        Path workspace = root.toFile().getCanonicalFile().toPath();
        byte[] bytes = Files.readAllBytes(Path.of(System.getProperty("nanobug.java.graph")));
        if (bytes.length > 32768) throw new IllegalArgumentException("Java project graph quota exceeded");
        JsonObject graph = JsonParser.parseString(new String(bytes, java.nio.charset.StandardCharsets.UTF_8)).getAsJsonObject();
        JsonArray nodes = graph.getAsJsonArray("projects");
        JsonArray libraries = graph.getAsJsonArray("libraries");
        if (nodes.size() == 0 || nodes.size() > 128 || libraries.size() > 1024)
            throw new IllegalArgumentException("Invalid Java project graph size");
        List<List<String>> sources = new ArrayList<>();
        // Check every source and reference before changing the private Eclipse workspace.
        for (int index = 0; index < nodes.size(); index++) {
            JsonObject node = nodes.get(index).getAsJsonObject();
            List<String> paths = new ArrayList<>();
            for (var source : node.getAsJsonArray("sources")) {
                String value = source.getAsString();
                Path path = workspace.resolve(value).toFile().getCanonicalFile().toPath();
                if (!path.startsWith(workspace)) throw new IllegalArgumentException("Java source escapes workspace");
                paths.add(value);
            }
            for (var reference : node.getAsJsonArray("references")) {
                int target = reference.getAsInt();
                if (target < 0 || target >= nodes.size() || target == index)
                    throw new IllegalArgumentException("Invalid Java project reference");
            }
            for (var library : node.getAsJsonArray("libraries"))
                if (library.getAsInt() < 0 || library.getAsInt() >= libraries.size())
                    throw new IllegalArgumentException("Invalid Java library reference");
            sources.add(paths);
        }
        List<IJavaProject> projects = new ArrayList<>();
        for (int index = 0; index < nodes.size(); index++) {
            IProject project = ResourcesPlugin.getWorkspace().getRoot().getProject("nanobug-project-" + index);
            if (!project.exists()) {
                ProjectsManager.createJavaProject(project, null, null, "bin", monitor);
            }
            if (!project.hasNature(UnmanagedFolderNature.NATURE_ID)) {
                var description = project.getDescription();
                var natures = new ArrayList<>(Arrays.asList(description.getNatureIds()));
                natures.add(UnmanagedFolderNature.NATURE_ID);
                description.setNatureIds(natures.toArray(new String[0]));
                project.setDescription(description, monitor);
            }
            projects.add(JavaCore.create(project));
        }
        for (int index = 0; index < nodes.size(); index++) {
            if (monitor.isCanceled()) throw new org.eclipse.core.runtime.OperationCanceledException();
            JsonObject node = nodes.get(index).getAsJsonObject();
            IJavaProject java = projects.get(index);
            IPath output = java.getProject().getFolder("bin").getFullPath();
            List<IClasspathEntry> entries = new ArrayList<>();
            // Link only this project's sources. Linking the whole workspace into every project
            // makes JDT's URI lookup pick an unrelated project with the same relative path length.
            for (int sourceIndex = 0; sourceIndex < sources.get(index).size(); sourceIndex++) {
                IFolder link = java.getProject().getFolder("source-" + sourceIndex);
                if (!link.exists()) link.createLink(workspace.resolve(sources.get(index).get(sourceIndex)).toUri(), IResource.REPLACE | IResource.ALLOW_MISSING_LOCAL, monitor);
                entries.add(JavaCore.newSourceEntry(link.getFullPath()));
            }
            for (var entry : java.getRawClasspath())
                if (entry.getEntryKind() == IClasspathEntry.CPE_CONTAINER) entries.add(entry);
            // Libraries are not exported: each consumer already has its exact mediated closure.
            for (var library : node.getAsJsonArray("libraries")) {
                IPath binary = org.eclipse.core.runtime.Path.fromOSString(libraries.get(library.getAsInt()).getAsString());
                entries.add(JavaCore.newLibraryEntry(binary, ProjectUtils.detectSources(binary.toFile().toPath()), null));
            }
            for (var reference : node.getAsJsonArray("references"))
                entries.add(JavaCore.newProjectEntry(projects.get(reference.getAsInt()).getPath(), false));
            java.setRawClasspath(entries.toArray(new IClasspathEntry[0]), output, monitor);
            String release = node.get("release").getAsString();
            String source = node.get("source").getAsString();
            String target = node.get("target").getAsString();
            String level = !release.isEmpty() ? release : !target.isEmpty() ? target : source;
            if (!level.isEmpty()) {
                var options = new HashMap<String,String>();
                String compliance = "8".equals(level) ? "1.8" : level;
                JavaCore.setComplianceOptions(compliance, options);
                options.put(JavaCore.COMPILER_RELEASE, release.isEmpty() ? JavaCore.DISABLED : JavaCore.ENABLED);
                if (release.isEmpty()) {
                    if (!source.isEmpty()) options.put(JavaCore.COMPILER_SOURCE, "8".equals(source) ? "1.8" : source);
                    if (!target.isEmpty()) options.put(JavaCore.COMPILER_CODEGEN_TARGET_PLATFORM, "8".equals(target) ? "1.8" : target);
                }
                java.setOptions(options);
            }
        }
        prepared = true;
    }
}
