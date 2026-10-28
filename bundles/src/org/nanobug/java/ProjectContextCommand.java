package org.nanobug.java;

import java.util.Arrays;
import java.util.LinkedHashMap;
import java.util.List;
import org.eclipse.core.resources.IFolder;
import org.eclipse.core.resources.IProject;
import org.eclipse.core.resources.IResource;
import org.eclipse.core.resources.ResourcesPlugin;
import org.eclipse.core.runtime.IPath;
import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.jdt.core.IJavaProject;
import org.eclipse.jdt.core.JavaCore;
import org.eclipse.jdt.ls.core.internal.IDelegateCommandHandler;
import org.eclipse.jdt.ls.core.internal.JavaLanguageServerPlugin;
import org.eclipse.jdt.ls.core.internal.ProjectUtils;
import org.eclipse.jdt.ls.core.internal.managers.InvisibleProjectImporter;
import org.eclipse.jdt.ls.core.internal.managers.ProjectsManager;
import org.eclipse.jdt.ls.core.internal.managers.UnmanagedFolderNature;

/** Explicit project metadata bypasses automatic build-file discovery without interpreting a build tool. */
public final class ProjectContextCommand implements IDelegateCommandHandler {
    /** Establish one private analysis project from the approved source/library preferences.
     * No POM, build script, user settings directory or application compiler output is modified.
     */
    @Override public Object executeCommand(String commandId, List<Object> arguments, IProgressMonitor monitor)
            throws Exception {
        if (!"nanobug.java.project.ready".equals(commandId) || arguments == null || !arguments.isEmpty())
            throw new IllegalArgumentException("Invalid Java project preparation");
        prepare(monitor);
        return null;
    }

    /** Initial import and the explicit readiness check share the same synchronous metadata path. */
    public static void prepare(IProgressMonitor monitor) throws Exception {
        var preferences = JavaLanguageServerPlugin.getPreferencesManager().getPreferences();
        var roots = preferences.getRootPaths();
        if (roots == null || roots.size() != 1)
            throw new IllegalArgumentException("An explicit Java context requires exactly one workspace");
        IPath root = roots.iterator().next();
        if (System.getProperty("nanobug.java.graph") != null) {
            ProjectGraph.prepare(root, monitor);
            return;
        }
        java.nio.file.Path canonicalRoot = root.toFile().getCanonicalFile().toPath();
        var configured = preferences.getInvisibleProjectSourcePaths();
        if (configured == null || configured.isEmpty())
            throw new IllegalArgumentException("An explicit Java context requires source directories");
        for (String source : configured) {
            var path = canonicalRoot.resolve(source).toFile().getCanonicalFile().toPath();
            if (!path.startsWith(canonicalRoot)) throw new IllegalArgumentException("Java source escapes workspace");
        }
        IProject project = ResourcesPlugin.getWorkspace().getRoot()
                .getProject(ProjectUtils.getWorkspaceInvisibleProjectName(root));
        if (!project.exists()) {
            // Keep all Eclipse metadata inside this context's private index, including its settings.
            ProjectsManager.createJavaProject(project, null, null, "bin", monitor);
            IFolder link = project.getFolder(ProjectUtils.WORKSPACE_LINK);
            link.createLink(root.toFile().toURI(), IResource.REPLACE, monitor);
        }
        if (!project.hasNature(UnmanagedFolderNature.NATURE_ID)) {
            var description = project.getDescription();
            var natures = new java.util.ArrayList<>(Arrays.asList(description.getNatureIds()));
            natures.add(UnmanagedFolderNature.NATURE_ID);
            description.setNatureIds(natures.toArray(new String[0]));
            project.setDescription(description, monitor);
        }
        IJavaProject java = JavaCore.create(project);
        IFolder link = project.getFolder(ProjectUtils.WORKSPACE_LINK);
        var sources = InvisibleProjectImporter.getSourcePaths(configured, link);
        var excluded = InvisibleProjectImporter.getExcludingPath(java, root, link);
        // Analysis bytecode stays private. Actual build outputs remain the build provider's responsibility.
        IPath output = project.getFolder("bin").getFullPath();
        java.setRawClasspath(InvisibleProjectImporter.resolveClassPathEntries(java, sources, excluded, output), output, monitor);
        var libraries = preferences.getReferencedLibraries();
        var binaries = ProjectUtils.collectBinaries(root, libraries.getInclude(), libraries.getExclude(), monitor);
        var entries = new LinkedHashMap<java.nio.file.Path, IPath>();
        for (var binary : binaries) entries.put(binary, ProjectUtils.detectSources(binary));
        ProjectUtils.updateBinaries(java, entries, monitor);
        if (monitor.isCanceled()) throw new org.eclipse.core.runtime.OperationCanceledException();
    }
}
