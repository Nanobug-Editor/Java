package org.nanobug.java;

import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import org.eclipse.core.runtime.IProgressMonitor;
import org.eclipse.core.runtime.OperationCanceledException;
import org.eclipse.jdt.core.ICompilationUnit;
import org.eclipse.jdt.core.WorkingCopyOwner;
import org.eclipse.jdt.core.dom.AST;
import org.eclipse.jdt.core.dom.ASTParser;
import org.eclipse.jdt.core.dom.CompilationUnit;
import org.eclipse.jdt.ls.core.internal.IDelegateCommandHandler;
import org.eclipse.jdt.ls.core.internal.JDTUtils;
import org.eclipse.jdt.ls.core.internal.JavaLanguageServerPlugin;
import org.eclipse.jdt.ls.core.internal.JobHelpers;
import org.eclipse.jdt.ls.core.internal.JSONUtility;
import org.eclipse.jdt.ls.core.internal.handlers.BaseDiagnosticsHandler;
import org.eclipse.jdt.ls.core.internal.handlers.BaseDocumentLifeCycleHandler;
import org.eclipse.lsp4j.jsonrpc.ResponseErrorException;
import org.eclipse.lsp4j.jsonrpc.messages.ResponseError;

/** Request-bound semantic diagnostics use JDT's working copy without publishing mutable editor state. */
public final class DiagnosticsCommand implements IDelegateCommandHandler {
    /**
     * Analyze the synchronized URI and expected UTF-8 SHA-256 in {@code arguments}.
     * {@code monitor} carries cancellation. The full report retains its source hash;
     * stale or unsynchronized buffers fail with ContentModified instead of returning guessed results.
     */
    @Override
    public Object executeCommand(String commandId, List<Object> arguments, IProgressMonitor monitor)
            throws Exception {
        if (!"nanobug.java.diagnostics".equals(commandId) || arguments == null || arguments.size() != 2) {
            throw new IllegalArgumentException("Invalid Java diagnostic query");
        }
        String uri = JSONUtility.toModel(arguments.get(0), String.class);
        String expected = JSONUtility.toModel(arguments.get(1), String.class);
        if (uri == null || expected == null || !expected.matches("[0-9a-f]{64}")) {
            throw new IllegalArgumentException("Invalid Java diagnostic source proof");
        }
        JobHelpers.waitForJobs(BaseDocumentLifeCycleHandler.DOCUMENT_LIFE_CYCLE_JOBS, monitor);
        if (monitor.isCanceled()) {
            throw new OperationCanceledException();
        }
        ICompilationUnit unit = JDTUtils.resolveCompilationUnit(uri);
        if (unit == null) {
            throw stale();
        }
        WorkingCopyOwner owner = JavaLanguageServerPlugin.getInstance().getWorkingCopyOwner();
        ICompilationUnit working = unit.findWorkingCopy(owner);
        if (working != null) {
            unit = working;
        }
        checkSource(unit, expected);
        ASTParser parser = ASTParser.newParser(AST.getJLSLatest());
        parser.setSource(unit);
        parser.setWorkingCopyOwner(owner);
        parser.setResolveBindings(true);
        parser.setBindingsRecovery(true);
        CompilationUnit tree = (CompilationUnit) parser.createAST(monitor);
        if (monitor.isCanceled()) {
            throw new OperationCanceledException();
        }
        checkSource(unit, expected);
        var diagnostics = BaseDiagnosticsHandler.toDiagnosticsArray(unit, Arrays.asList(tree.getProblems()), true);
        // Conversion also reads the buffer for UTF-16 coordinates; retain the same snapshot proof.
        checkSource(unit, expected);
        return Map.of("kind", "full", "items", diagnostics, "sourceHash", expected);
    }

    /** Compare the current working-copy bytes with {@code expected}; no source copy is retained. */
    private static void checkSource(ICompilationUnit unit, String expected) throws Exception {
        String source = unit.getBuffer().getContents();
        String actual = HexFormat.of().formatHex(MessageDigest.getInstance("SHA-256")
                .digest(source.getBytes(StandardCharsets.UTF_8)));
        if (!expected.equals(actual)) {
            throw stale();
        }
    }

    /** Use the standard LSP stale-content category; callers may retry a newly captured snapshot. */
    private static ResponseErrorException stale() {
        return new ResponseErrorException(new ResponseError(-32801,
                "Java source snapshot is not synchronized", null));
    }
}
