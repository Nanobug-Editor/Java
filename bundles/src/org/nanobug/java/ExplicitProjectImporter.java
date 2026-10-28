package org.nanobug.java;

import org.eclipse.core.runtime.CoreException;
import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.core.runtime.IStatus;
import org.eclipse.core.runtime.Status;
import org.eclipse.jdt.ls.core.internal.AbstractProjectImporter;

/** Create the approved generic project during initialization, before any source working copy. */
public final class ExplicitProjectImporter extends AbstractProjectImporter {
    /** Only an explicitly prepared Java context opts in; ordinary source discovery stays unchanged. */
    @Override public boolean applies(IProgressMonitor monitor) {
        return Boolean.getBoolean("nanobug.java.explicitProject")
                && getPreferences().getInvisibleProjectSourcePaths() != null
                && !getPreferences().getInvisibleProjectSourcePaths().isEmpty();
    }
    /** No build file is read and no build tool runs; metadata and bytecode remain in the private index. */
    @Override public void importToWorkspace(IProgressMonitor monitor) throws CoreException {
        try {
            ProjectContextCommand.prepare(monitor);
            directories = java.util.Collections.singleton(rootFolder.toPath());
        } catch (CoreException error) { throw error; }
        catch (Exception error) { throw new CoreException(new Status(IStatus.ERROR, "org.nanobug.java.diagnostics", "Cannot prepare explicit Java project", error)); }
    }
    /** A reused importer must not retain discovery facts for another root. */
    @Override public void reset() { directories = null; }
}
