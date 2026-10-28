package org.nanobug.java;

import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.core.runtime.OperationCanceledException;
import org.eclipse.jdt.core.IClassFile;
import org.eclipse.jdt.ls.core.internal.managers.InvisibleProjectBuildSupport;

/** Plain Java projects use local attachments and decompilation without build-tool source discovery. */
public final class LocalSourcesOnly extends InvisibleProjectBuildSupport {
    /**
     * Keep the inherited plain-project classpath behavior, but never discover remote sources for
     * {@code classFile}. {@code monitor} still cancels the caller; missing attachments fall through
     * to JDT's packaged decompiler. Build-tool discovery belongs to a separate project provider.
     */
    @Override
    public void discoverSource(IClassFile classFile, IProgressMonitor monitor) {
        if (monitor.isCanceled()) {
            throw new OperationCanceledException();
        }
    }
}
