// AlSemantic — .NET bridge for AL CodeAnalysis API
//
// Runs as a subprocess. Reads JSON-RPC requests from stdin, writes responses to stdout.
// Loads CodeAnalysis.dll from the AL Tool installation path (passed as first argument).
//
// Commands:
//   ping        — health check
//   analyze     — run DiagnosticAnalyzers on source
//   compile     — invoke alc.exe compiler
//   typeAt      — resolve symbol at position
//   completions — get completion items at position
//   builtins    — extract all built-in types and methods
//   errorCodes  — list all compiler error codes
//   shutdown    — exit cleanly

using System.Collections.Immutable;
using System.Diagnostics;
using System.Reflection;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace AlSemantic;

class Program
{
    private static readonly JsonSerializerOptions JsonOptions = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        WriteIndented = false,
    };

    static async Task Main(string[] args)
    {
        if (args.Length < 1)
        {
            Console.Error.WriteLine("Usage: AlSemantic <path-to-CodeAnalysis.dll>");
            Environment.Exit(1);
        }

        var codeAnalysisPath = args[0];

        if (!File.Exists(codeAnalysisPath))
        {
            Console.Error.WriteLine($"CodeAnalysis.dll not found at: {codeAnalysisPath}");
            Environment.Exit(1);
        }

        // Set up assembly resolution for the AL extension's directory so that
        // dependent DLLs (e.g. System.Collections.Immutable) are found.
        var alExtDir = Path.GetDirectoryName(codeAnalysisPath) ?? ".";
        AppDomain.CurrentDomain.AssemblyResolve += (sender, resolveArgs) =>
        {
            var name = new AssemblyName(resolveArgs.Name);
            var candidate = Path.Combine(alExtDir, name.Name + ".dll");
            if (File.Exists(candidate))
            {
                try { return Assembly.LoadFrom(candidate); }
                catch { /* fall through */ }
            }
            return null;
        };

        // Load CodeAnalysis assembly
        Assembly? codeAnalysis = null;
        try
        {
            codeAnalysis = Assembly.LoadFrom(codeAnalysisPath);
            Console.Error.WriteLine($"Loaded CodeAnalysis from: {codeAnalysisPath}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to load CodeAnalysis.dll: {ex.Message}");
            Environment.Exit(1);
        }

        var bridge = new CodeAnalysisBridge(codeAnalysis!, alExtDir);

        // JSON-RPC loop: read line-delimited JSON from stdin
        using var reader = new StreamReader(Console.OpenStandardInput());

        while (true)
        {
            var line = await reader.ReadLineAsync();
            if (line == null) break; // EOF — parent process closed stdin

            try
            {
                var request = JsonSerializer.Deserialize<JsonElement>(line);
                var method = request.GetProperty("method").GetString() ?? "";
                var id = request.GetProperty("id").GetUInt64();
                var hasParams = request.TryGetProperty("params", out var paramsElement);

                object? result;
                object? error = null;

                try
                {
                    result = method switch
                    {
                        "ping" => HandlePing(),
                        "analyze" => bridge.HandleAnalyze(paramsElement),
                        "compile" => bridge.HandleCompile(paramsElement),
                        "typeAt" => bridge.HandleTypeAt(paramsElement),
                        "completions" => bridge.HandleCompletions(paramsElement),
                        "builtins" => bridge.HandleBuiltins(),
                        "errorCodes" => bridge.HandleErrorCodes(),
                        "shutdown" => HandleShutdown(),
                        _ => throw new RpcException(-32601, $"Unknown method: {method}")
                    };
                }
                catch (RpcException ex)
                {
                    result = null;
                    error = new { code = ex.Code, message = ex.Message };
                }
                catch (Exception ex)
                {
                    result = null;
                    error = new { code = -32603, message = $"Internal error: {ex.Message}" };
                    Console.Error.WriteLine($"Error in {method}: {ex}");
                }

                if (error != null)
                {
                    var errorResponse = new { id, error };
                    Console.WriteLine(JsonSerializer.Serialize(errorResponse, JsonOptions));
                }
                else
                {
                    var successResponse = new { id, result };
                    Console.WriteLine(JsonSerializer.Serialize(successResponse, JsonOptions));
                }
                Console.Out.Flush();
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Error processing request: {ex.Message}");
                // Don't crash — continue reading
            }
        }
    }

    /// <summary>Returns a simple health-check response.</summary>
    static object HandlePing()
    {
        return new { status = "ok" };
    }

    /// <summary>Schedules a graceful process exit after sending the response.</summary>
    static object HandleShutdown()
    {
        _ = Task.Run(async () =>
        {
            await Task.Delay(100);
            Environment.Exit(0);
        });
        return new { status = "shutting_down" };
    }
}

/// <summary>Custom exception for RPC errors with error codes.</summary>
class RpcException : Exception
{
    public int Code { get; }

    public RpcException(int code, string message) : base(message)
    {
        Code = code;
    }
}

/// <summary>
/// Bridge to CodeAnalysis functionality via reflection.
///
/// All interaction with the Microsoft.Dynamics.Nav.CodeAnalysis assembly is done
/// through reflection because we cannot reference the DLL at compile time — it
/// ships as part of the AL Language VS Code extension and its API is not public.
///
/// The patterns used here are derived from:
///   - BusinessCentral.LinterCop (StefanMaron) — DiagnosticAnalyzer patterns
///   - al-code-outline (anzwdev) — Compilation and SyntaxTree creation
/// </summary>
class CodeAnalysisBridge
{
    private readonly Assembly _codeAnalysis;
    private readonly string _alExtDir;

    // Cached types from CodeAnalysis — resolved once at construction
    private readonly Type? _syntaxTreeType;
    private readonly Type? _sourceTextType;
    private readonly Type? _compilationType;
    private readonly Type? _compilationOptionsType;
    private readonly Type? _diagnosticDescriptorType;
    private readonly Type? _diagnosticSeverityType;
    private readonly Type? _diagnosticType;
    private readonly Type? _diagnosticAnalyzerType;
    private readonly Type? _navTypeKindEnum;
    private readonly Type? _errorCodeEnum;
    private readonly Type? _navDiagnosticInfoType;
    private readonly Type? _parseOptionsType;
    private readonly Type? _symbolKindEnum;
    private readonly Type? _compilationUnitSyntaxType;
    private readonly Type? _syntaxNodeType;
    private readonly Type? _textSpanType;
    private readonly Type? _linePositionSpanType;

    // Cached methods
    private readonly MethodInfo? _parseObjectTextMethod;
    private readonly MethodInfo? _sourceTextFromStringMethod;
    private readonly MethodInfo? _getCompilationUnitRootMethod;
    private readonly MethodInfo? _getDiagnosticsMethod;

    public CodeAnalysisBridge(Assembly codeAnalysis, string alExtDir)
    {
        _codeAnalysis = codeAnalysis;
        _alExtDir = alExtDir;

        // Resolve all commonly used types once
        _syntaxTreeType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.SyntaxTree");
        _sourceTextType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Text.SourceText");
        _compilationType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Compilation");
        _compilationOptionsType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.CompilationOptions");
        _diagnosticDescriptorType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.DiagnosticDescriptor");
        _diagnosticSeverityType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.DiagnosticSeverity");
        _diagnosticType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.Diagnostic");
        _diagnosticAnalyzerType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.DiagnosticAnalyzer");
        _navTypeKindEnum = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.NavTypeKind");
        _errorCodeEnum = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.ErrorCode");
        _navDiagnosticInfoType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.NavDiagnosticInfo");
        _parseOptionsType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.ParseOptions");
        _symbolKindEnum = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.SymbolKind");
        _compilationUnitSyntaxType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.CompilationUnitSyntax");
        _syntaxNodeType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.SyntaxNode");
        _textSpanType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Text.TextSpan");
        _linePositionSpanType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Text.LinePositionSpan");

        // Resolve commonly used methods
        _sourceTextFromStringMethod = ResolveSourceTextFrom();
        _parseObjectTextMethod = ResolveParseObjectText();
        _getCompilationUnitRootMethod = _syntaxTreeType?.GetMethod("GetCompilationUnitRoot",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?? _syntaxTreeType?.GetMethod("GetRoot",
                BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
        _getDiagnosticsMethod = _syntaxTreeType?.GetMethod("GetDiagnostics",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);

        LogFoundTypes();
    }

    /// <summary>Log which types were found for diagnostic purposes.</summary>
    private void LogFoundTypes()
    {
        var types = new (string name, Type? type)[]
        {
            ("SyntaxTree", _syntaxTreeType),
            ("SourceText", _sourceTextType),
            ("Compilation", _compilationType),
            ("DiagnosticDescriptor", _diagnosticDescriptorType),
            ("DiagnosticAnalyzer", _diagnosticAnalyzerType),
            ("NavTypeKind", _navTypeKindEnum),
            ("ErrorCode", _errorCodeEnum),
            ("NavDiagnosticInfo", _navDiagnosticInfoType),
            ("ParseOptions", _parseOptionsType),
        };

        foreach (var (name, type) in types)
        {
            Console.Error.WriteLine($"  {name}: {(type != null ? "found" : "NOT FOUND")}");
        }
    }

    // -----------------------------------------------------------------------
    // Type / method resolution helpers
    // -----------------------------------------------------------------------

    private Type? FindType(string fullName)
    {
        try
        {
            return _codeAnalysis.GetType(fullName);
        }
        catch
        {
            return null;
        }
    }

    private IEnumerable<Type> FindTypes(string namespacePrefix)
    {
        try
        {
            return _codeAnalysis.GetTypes()
                .Where(t => t.Namespace?.StartsWith(namespacePrefix) == true);
        }
        catch (ReflectionTypeLoadException ex)
        {
            return ex.Types.Where(t => t != null &&
                t.Namespace?.StartsWith(namespacePrefix) == true)!;
        }
    }

    /// <summary>
    /// Resolve SourceText.From(string) — the method has several overloads.
    /// </summary>
    private MethodInfo? ResolveSourceTextFrom()
    {
        if (_sourceTextType == null) return null;

        // Try From(string)
        var method = _sourceTextType.GetMethod("From",
            BindingFlags.Static | BindingFlags.Public,
            null, new[] { typeof(string) }, null);
        if (method != null) return method;

        // Try From(string, Encoding) with a default
        var methods = _sourceTextType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "From");
        foreach (var m in methods)
        {
            var pars = m.GetParameters();
            if (pars.Length >= 1 && pars[0].ParameterType == typeof(string))
                return m;
        }

        return null;
    }

    /// <summary>
    /// Resolve SyntaxTree.ParseObjectText — various overloads exist.
    /// </summary>
    private MethodInfo? ResolveParseObjectText()
    {
        if (_syntaxTreeType == null) return null;

        // Look for ParseObjectText methods
        var methods = _syntaxTreeType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "ParseObjectText")
            .OrderBy(m => m.GetParameters().Length)
            .ToArray();

        return methods.FirstOrDefault();
    }

    /// <summary>
    /// Parse AL source code into a SyntaxTree using the CodeAnalysis API.
    /// Returns null if parsing is not available.
    /// </summary>
    private object? ParseSource(string source, string filePath)
    {
        if (_parseObjectTextMethod == null || _sourceTextFromStringMethod == null)
            return null;

        // Create SourceText from the string
        object? sourceText;
        var fromParams = _sourceTextFromStringMethod.GetParameters();
        if (fromParams.Length == 1)
        {
            sourceText = _sourceTextFromStringMethod.Invoke(null, new object[] { source });
        }
        else
        {
            // Fill extra params with defaults
            var args = new object?[fromParams.Length];
            args[0] = source;
            for (int i = 1; i < fromParams.Length; i++)
            {
                if (fromParams[i].HasDefaultValue)
                    args[i] = fromParams[i].DefaultValue;
                else if (fromParams[i].ParameterType == typeof(Encoding))
                    args[i] = Encoding.UTF8;
                else
                    args[i] = null;
            }
            sourceText = _sourceTextFromStringMethod.Invoke(null, args);
        }

        if (sourceText == null) return null;

        // Call ParseObjectText
        var parseParams = _parseObjectTextMethod.GetParameters();
        var parseArgs = new object?[parseParams.Length];

        for (int i = 0; i < parseParams.Length; i++)
        {
            var pType = parseParams[i].ParameterType;
            if (pType == _sourceTextType || pType.IsAssignableFrom(sourceText.GetType()))
                parseArgs[i] = sourceText;
            else if (pType == typeof(string))
                parseArgs[i] = filePath;
            else if (parseParams[i].HasDefaultValue)
                parseArgs[i] = parseParams[i].DefaultValue;
            else
                parseArgs[i] = null;
        }

        return _parseObjectTextMethod.Invoke(null, parseArgs);
    }

    /// <summary>
    /// Extract diagnostics from a SyntaxTree or Compilation object.
    /// </summary>
    private List<object> ExtractDiagnostics(object diagSource, string defaultFile)
    {
        var results = new List<object>();

        // Get diagnostics via GetDiagnostics()
        var sourceType = diagSource.GetType();
        var getDiag = sourceType.GetMethod("GetDiagnostics",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);

        if (getDiag == null)
        {
            // Try with CancellationToken parameter
            getDiag = sourceType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
                .FirstOrDefault(m => m.Name == "GetDiagnostics");
        }

        if (getDiag == null) return results;

        object? diagnosticsObj;
        var diagParams = getDiag.GetParameters();
        if (diagParams.Length == 0)
        {
            diagnosticsObj = getDiag.Invoke(diagSource, null);
        }
        else
        {
            var args = new object?[diagParams.Length];
            for (int i = 0; i < diagParams.Length; i++)
            {
                if (diagParams[i].HasDefaultValue)
                    args[i] = diagParams[i].DefaultValue;
                else
                    args[i] = null;
            }
            diagnosticsObj = getDiag.Invoke(diagSource, args);
        }

        if (diagnosticsObj is not System.Collections.IEnumerable enumerable)
            return results;

        foreach (var diag in enumerable)
        {
            if (diag == null) continue;
            try
            {
                results.Add(ConvertSingleDiagnostic(diag, defaultFile));
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Skipping diagnostic: {ex.Message}");
            }
        }

        return results;
    }

    /// <summary>
    /// Convert a single Diagnostic object to our JSON-friendly format.
    /// Handles location extraction with line/column positions.
    /// </summary>
    private object ConvertSingleDiagnostic(object diag, string defaultFile)
    {
        var diagType = diag.GetType();

        // Extract ID
        var id = GetPropertyValue<string>(diag, diagType, "Id") ?? "";

        // Extract message — try GetMessage() first, then Message property
        var message = "";
        var getMessageMethod = diagType.GetMethod("GetMessage",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
        if (getMessageMethod != null)
        {
            message = getMessageMethod.Invoke(diag, null)?.ToString() ?? "";
        }
        if (string.IsNullOrEmpty(message))
        {
            message = GetPropertyValue<string>(diag, diagType, "Message") ?? "";
        }

        // Extract severity
        var severityObj = GetPropertyValue(diag, diagType, "Severity");
        var severity = severityObj?.ToString() ?? "Warning";

        // Extract location with line/column positions
        uint line = 0, column = 0, endLine = 0, endColumn = 0;
        var file = defaultFile;

        var location = GetPropertyValue(diag, diagType, "Location");
        if (location != null)
        {
            try
            {
                var locType = location.GetType();

                // Try GetLineSpan() to get FileLinePositionSpan
                var getLineSpan = locType.GetMethod("GetLineSpan",
                    BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                var lineSpanObj = getLineSpan?.Invoke(location, null);

                if (lineSpanObj == null)
                {
                    var getMapped = locType.GetMethod("GetMappedLineSpan",
                        BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                    lineSpanObj = getMapped?.Invoke(location, null);
                }

                if (lineSpanObj != null)
                {
                    var spanType = lineSpanObj.GetType();

                    // Extract file path from the span
                    var pathProp = spanType.GetProperty("Path");
                    var spanPath = pathProp?.GetValue(lineSpanObj)?.ToString();
                    if (!string.IsNullOrEmpty(spanPath))
                        file = spanPath;

                    // Extract start position
                    var startProp = spanType.GetProperty("StartLinePosition");
                    var startPos = startProp?.GetValue(lineSpanObj);
                    if (startPos != null)
                    {
                        var posType = startPos.GetType();
                        line = (uint)(GetPropertyValue<int>(startPos, posType, "Line"));
                        column = (uint)(GetPropertyValue<int>(startPos, posType, "Character"));
                    }

                    // Extract end position
                    var endProp = spanType.GetProperty("EndLinePosition");
                    var endPos = endProp?.GetValue(lineSpanObj);
                    if (endPos != null)
                    {
                        var posType = endPos.GetType();
                        endLine = (uint)(GetPropertyValue<int>(endPos, posType, "Line"));
                        endColumn = (uint)(GetPropertyValue<int>(endPos, posType, "Character"));
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Location extraction failed: {ex.Message}");
            }
        }

        return new
        {
            file,
            line,
            column,
            endLine,
            endColumn,
            severity = NormalizeSeverity(severity),
            code = id,
            message,
        };
    }

    /// <summary>Normalize severity strings to lowercase LSP-compatible values.</summary>
    private static string NormalizeSeverity(string severity)
    {
        return severity.ToLowerInvariant() switch
        {
            "error" => "error",
            "warning" => "warning",
            "info" or "information" => "info",
            "hidden" or "hint" => "hint",
            _ => "warning",
        };
    }

    // -----------------------------------------------------------------------
    // analyze — run DiagnosticAnalyzers on source code
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "analyze" request. Parses the source file into a SyntaxTree
    /// and optionally runs DiagnosticAnalyzers from the specified analyzer DLLs.
    ///
    /// Approach:
    ///   1. Parse source with SyntaxTree.ParseObjectText()
    ///   2. Collect syntax-level diagnostics (parse errors)
    ///   3. If analyzer DLLs are provided, load them and run their analyzers
    ///      against a Compilation built from the parsed source
    ///   4. Return all collected diagnostics
    /// </summary>
    public object HandleAnalyze(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var source = @params.GetProperty("source").GetString()
            ?? throw new RpcException(-32602, "Missing 'source' parameter");

        // Read optional parameters
        var analyzers = new List<string>();
        if (@params.TryGetProperty("analyzers", out var analyzersElem) && analyzersElem.ValueKind == JsonValueKind.Array)
        {
            foreach (var item in analyzersElem.EnumerateArray())
            {
                var name = item.GetString();
                if (name != null) analyzers.Add(name);
            }
        }

        string packageCache = "";
        if (@params.TryGetProperty("packageCache", out var pkgElem))
        {
            packageCache = pkgElem.GetString() ?? "";
        }
        // Also check snake_case from Rust serialization
        if (string.IsNullOrEmpty(packageCache) && @params.TryGetProperty("package_cache", out var pkgElem2))
        {
            packageCache = pkgElem2.GetString() ?? "";
        }

        try
        {
            return RunAnalysis(file, source, analyzers, packageCache);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Analyzer error: {ex}");
            return Array.Empty<object>();
        }
    }

    private object RunAnalysis(string file, string source, List<string> analyzerPaths, string packageCache)
    {
        // Step 1: Parse the source into a SyntaxTree
        var tree = ParseSource(source, file);
        if (tree == null)
        {
            throw new RpcException(-32000,
                "Cannot parse source: SyntaxTree.ParseObjectText or SourceText.From not found in CodeAnalysis");
        }

        // Step 2: Collect syntax-level diagnostics (parse errors)
        var allDiagnostics = ExtractDiagnostics(tree, file);
        Console.Error.WriteLine($"  Syntax diagnostics: {allDiagnostics.Count}");

        // Step 3: Try to create a Compilation and run analyzers
        if (analyzerPaths.Count > 0 && _compilationType != null)
        {
            try
            {
                var compilation = CreateCompilation(new[] { tree }, packageCache);
                if (compilation != null)
                {
                    var analyzerDiags = RunLoadedAnalyzers(compilation, analyzerPaths);
                    allDiagnostics.AddRange(analyzerDiags);
                    Console.Error.WriteLine($"  Analyzer diagnostics: {analyzerDiags.Count}");
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Compilation/analyzer error (non-fatal): {ex.Message}");
                // Syntax diagnostics are still returned
            }
        }

        return allDiagnostics;
    }

    /// <summary>
    /// Create a Compilation object from syntax trees.
    /// Mirrors the pattern from anzwdev/al-code-outline's ALProjectCompilation.
    /// </summary>
    private object? CreateCompilation(object[] syntaxTrees, string packageCache)
    {
        if (_compilationType == null) return null;

        // Find the Compilation.Create static method
        var createMethods = _compilationType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "Create")
            .OrderByDescending(m => m.GetParameters().Length)
            .ToArray();

        if (createMethods.Length == 0)
        {
            Console.Error.WriteLine("  Compilation.Create method not found");
            return null;
        }

        // Try each overload until one works
        foreach (var createMethod in createMethods)
        {
            try
            {
                var compilation = TryCreateCompilation(createMethod, syntaxTrees);
                if (compilation != null)
                {
                    // Try to add package references if we have a package cache
                    if (!string.IsNullOrEmpty(packageCache) && Directory.Exists(packageCache))
                    {
                        compilation = TryAddReferences(compilation, packageCache);
                    }
                    return compilation;
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Compilation.Create overload failed: {ex.Message}");
            }
        }

        return null;
    }

    /// <summary>
    /// Try to invoke a specific Compilation.Create overload.
    /// </summary>
    private object? TryCreateCompilation(MethodInfo createMethod, object[] syntaxTrees)
    {
        var pars = createMethod.GetParameters();
        var args = new object?[pars.Length];

        for (int i = 0; i < pars.Length; i++)
        {
            var pType = pars[i].ParameterType;
            var pName = pars[i].Name?.ToLowerInvariant() ?? "";

            if (pType == typeof(string))
            {
                // name, publisher, version parameters
                if (pName.Contains("name") || pName.Contains("assembly"))
                    args[i] = "AlSemanticAnalysis";
                else if (pName.Contains("publisher"))
                    args[i] = "AlSemantic";
                else
                    args[i] = "0.0.0.0";
            }
            else if (pType == typeof(Guid) || pType == typeof(Guid?))
            {
                args[i] = Guid.Empty;
            }
            else if (pType == typeof(Version))
            {
                args[i] = new Version(0, 0, 0, 0);
            }
            else if (pType.IsGenericType && pType.GetGenericTypeDefinition() == typeof(IEnumerable<>))
            {
                // This is likely the syntaxTrees parameter
                var elementType = pType.GetGenericArguments()[0];
                if (elementType == _syntaxTreeType || (syntaxTrees.Length > 0 && elementType.IsAssignableFrom(syntaxTrees[0].GetType())))
                {
                    // Create typed array
                    var typedArray = Array.CreateInstance(elementType, syntaxTrees.Length);
                    for (int j = 0; j < syntaxTrees.Length; j++)
                        typedArray.SetValue(syntaxTrees[j], j);
                    args[i] = typedArray;
                }
                else
                {
                    args[i] = null;
                }
            }
            else if (_compilationOptionsType != null && pType == _compilationOptionsType)
            {
                args[i] = CreateDefaultCompilationOptions();
            }
            else if (pars[i].HasDefaultValue)
            {
                args[i] = pars[i].DefaultValue;
            }
            else if (!pType.IsValueType)
            {
                args[i] = null;
            }
            else
            {
                args[i] = Activator.CreateInstance(pType);
            }
        }

        return createMethod.Invoke(null, args);
    }

    /// <summary>
    /// Create default CompilationOptions.
    /// </summary>
    private object? CreateDefaultCompilationOptions()
    {
        if (_compilationOptionsType == null) return null;

        try
        {
            // Try constructor with minimal parameters
            var constructors = _compilationOptionsType.GetConstructors(
                BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);

            // Sort by parameter count, try simplest first
            foreach (var ctor in constructors.OrderBy(c => c.GetParameters().Length))
            {
                try
                {
                    var pars = ctor.GetParameters();
                    var args = new object?[pars.Length];
                    for (int i = 0; i < pars.Length; i++)
                    {
                        if (pars[i].HasDefaultValue)
                            args[i] = pars[i].DefaultValue;
                        else if (pars[i].ParameterType.IsEnum)
                            args[i] = Enum.GetValues(pars[i].ParameterType).GetValue(0);
                        else if (!pars[i].ParameterType.IsValueType)
                            args[i] = null;
                        else
                            args[i] = Activator.CreateInstance(pars[i].ParameterType);
                    }
                    return ctor.Invoke(args);
                }
                catch
                {
                    continue;
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  CompilationOptions creation failed: {ex.Message}");
        }

        return null;
    }

    /// <summary>
    /// Try to add .app package references to a Compilation.
    /// </summary>
    private object TryAddReferences(object compilation, string packageCache)
    {
        try
        {
            // Look for LocalCacheSymbolReferenceLoader
            var loaderType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.CommandLine.LocalCacheSymbolReferenceLoader");
            if (loaderType == null)
            {
                Console.Error.WriteLine("  LocalCacheSymbolReferenceLoader not found");
                return compilation;
            }

            // Create loader — constructor takes IEnumerable<string>
            var ctors = loaderType.GetConstructors(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance);
            object? loader = null;

            foreach (var ctor in ctors)
            {
                try
                {
                    var pars = ctor.GetParameters();
                    var args = new object?[pars.Length];
                    args[0] = new List<string> { packageCache };
                    for (int i = 1; i < pars.Length; i++)
                    {
                        if (pars[i].HasDefaultValue)
                            args[i] = pars[i].DefaultValue;
                        else
                            args[i] = null;
                    }
                    loader = ctor.Invoke(args);
                    break;
                }
                catch
                {
                    continue;
                }
            }

            if (loader == null)
            {
                Console.Error.WriteLine("  Could not create reference loader");
                return compilation;
            }

            // Call compilation.WithReferenceLoader(loader)
            var withRefLoader = compilation.GetType().GetMethod("WithReferenceLoader",
                BindingFlags.Instance | BindingFlags.Public);
            if (withRefLoader != null)
            {
                compilation = withRefLoader.Invoke(compilation, new[] { loader }) ?? compilation;
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Reference loading failed (non-fatal): {ex.Message}");
        }

        return compilation;
    }

    /// <summary>
    /// Load analyzer DLLs and run them against a Compilation.
    /// Analyzers are loaded from the AL extension directory by name (e.g., "CodeCop").
    /// </summary>
    private List<object> RunLoadedAnalyzers(object compilation, List<string> analyzerNames)
    {
        var results = new List<object>();

        if (_diagnosticAnalyzerType == null)
        {
            Console.Error.WriteLine("  DiagnosticAnalyzer type not found, skipping analyzers");
            return results;
        }

        foreach (var analyzerName in analyzerNames)
        {
            try
            {
                // Resolve analyzer DLL path
                var dllPath = ResolveAnalyzerPath(analyzerName);
                if (dllPath == null)
                {
                    Console.Error.WriteLine($"  Analyzer not found: {analyzerName}");
                    continue;
                }

                Console.Error.WriteLine($"  Loading analyzer: {dllPath}");
                var analyzerAssembly = Assembly.LoadFrom(dllPath);

                // Find all DiagnosticAnalyzer subclasses in the assembly
                var analyzerTypes = analyzerAssembly.GetTypes()
                    .Where(t => !t.IsAbstract && _diagnosticAnalyzerType.IsAssignableFrom(t));

                foreach (var analyzerType in analyzerTypes)
                {
                    try
                    {
                        var analyzer = Activator.CreateInstance(analyzerType);
                        if (analyzer == null) continue;

                        // Run the analyzer via Compilation.GetAnalyzerDiagnostics or
                        // CompilationWithAnalyzers
                        var diags = RunSingleAnalyzer(compilation, analyzer);
                        results.AddRange(diags);
                    }
                    catch (Exception ex)
                    {
                        Console.Error.WriteLine($"  Analyzer {analyzerType.Name} failed: {ex.Message}");
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Failed to load analyzer {analyzerName}: {ex.Message}");
            }
        }

        return results;
    }

    /// <summary>Resolve an analyzer name to its DLL path.</summary>
    private string? ResolveAnalyzerPath(string analyzerName)
    {
        // If it's already a full path, use it
        if (File.Exists(analyzerName)) return analyzerName;

        // Try common patterns in the AL extension directory
        var candidates = new[]
        {
            Path.Combine(_alExtDir, $"Microsoft.Dynamics.Nav.Analyzers.{analyzerName}.dll"),
            Path.Combine(_alExtDir, $"{analyzerName}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"Microsoft.Dynamics.Nav.Analyzers.{analyzerName}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"{analyzerName}.dll"),
        };

        return candidates.FirstOrDefault(File.Exists);
    }

    /// <summary>
    /// Run a single DiagnosticAnalyzer instance against a Compilation.
    /// Uses CompilationWithAnalyzers if available, otherwise falls back to
    /// direct invocation.
    /// </summary>
    private List<object> RunSingleAnalyzer(object compilation, object analyzer)
    {
        var results = new List<object>();

        // Try using CompilationWithAnalyzers API
        var cwAnalyzersType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.CompilationWithAnalyzers");
        if (cwAnalyzersType != null)
        {
            try
            {
                // Create ImmutableArray<DiagnosticAnalyzer> with our single analyzer
                var immArrayType = typeof(ImmutableArray);
                var createMethod = immArrayType.GetMethods(BindingFlags.Static | BindingFlags.Public)
                    .FirstOrDefault(m => m.Name == "Create" && m.GetParameters().Length == 1
                        && m.GetParameters()[0].ParameterType.IsArray);

                if (createMethod != null && _diagnosticAnalyzerType != null)
                {
                    var genericCreate = createMethod.MakeGenericMethod(_diagnosticAnalyzerType);
                    var analyzerArray = Array.CreateInstance(_diagnosticAnalyzerType, 1);
                    analyzerArray.SetValue(analyzer, 0);
                    var immAnalyzers = genericCreate.Invoke(null, new object[] { analyzerArray });

                    // Create CompilationWithAnalyzers
                    var ctors = cwAnalyzersType.GetConstructors(BindingFlags.Public | BindingFlags.Instance);
                    foreach (var ctor in ctors)
                    {
                        try
                        {
                            var pars = ctor.GetParameters();
                            var args = new object?[pars.Length];

                            for (int i = 0; i < pars.Length; i++)
                            {
                                if (pars[i].ParameterType == _compilationType)
                                    args[i] = compilation;
                                else if (pars[i].ParameterType.Name.Contains("ImmutableArray"))
                                    args[i] = immAnalyzers;
                                else if (pars[i].HasDefaultValue)
                                    args[i] = pars[i].DefaultValue;
                                else
                                    args[i] = null;
                            }

                            var cwa = ctor.Invoke(args);

                            // Call GetAnalyzerDiagnosticsAsync() or GetAllDiagnosticsAsync()
                            var getDiagsAsync = cwAnalyzersType.GetMethod("GetAnalyzerDiagnosticsAsync",
                                BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                                ?? cwAnalyzersType.GetMethod("GetAllDiagnosticsAsync",
                                    BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);

                            if (getDiagsAsync != null)
                            {
                                var task = getDiagsAsync.Invoke(cwa, null);
                                if (task != null)
                                {
                                    // Await the Task<ImmutableArray<Diagnostic>>
                                    var waitMethod = task.GetType().GetMethod("Wait",
                                        BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                                    waitMethod?.Invoke(task, null);

                                    var resultProp = task.GetType().GetProperty("Result");
                                    var diagResult = resultProp?.GetValue(task);

                                    if (diagResult is System.Collections.IEnumerable diagEnum)
                                    {
                                        foreach (var d in diagEnum)
                                        {
                                            if (d == null) continue;
                                            try
                                            {
                                                results.Add(ConvertSingleDiagnostic(d, ""));
                                            }
                                            catch { /* skip */ }
                                        }
                                    }
                                }
                            }

                            return results;
                        }
                        catch
                        {
                            continue;
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  CompilationWithAnalyzers failed: {ex.Message}");
            }
        }

        // Fallback: get diagnostics from the Compilation itself
        var compDiags = ExtractDiagnostics(compilation, "");
        results.AddRange(compDiags);

        return results;
    }

    // -----------------------------------------------------------------------
    // compile — invoke alc.exe as a subprocess
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "compile" request. Runs alc.exe (the AL compiler) as a subprocess
    /// with error logging enabled, then parses the output for diagnostics.
    ///
    /// Expected params:
    ///   - project: path to the project directory (must contain app.json)
    ///   - alcPath (optional): path to alc.dll, otherwise discovered from alExtDir
    ///   - packageCachePath (optional): path to .alpackages
    /// </summary>
    public object HandleCompile(JsonElement @params)
    {
        var project = @params.GetProperty("project").GetString()
            ?? throw new RpcException(-32602, "Missing 'project' parameter");

        if (!Directory.Exists(project))
        {
            throw new RpcException(-32001, $"Project directory not found: {project}");
        }

        // Resolve alc path
        var alcPath = "";
        if (@params.TryGetProperty("alcPath", out var alcElem))
            alcPath = alcElem.GetString() ?? "";
        if (@params.TryGetProperty("alc_path", out var alcElem2))
            alcPath = alcElem2.GetString() ?? "";

        if (string.IsNullOrEmpty(alcPath))
        {
            // Try to find alc in the AL extension directory
            var candidates = new[]
            {
                Path.Combine(_alExtDir, "bin", "alc.dll"),
                Path.Combine(_alExtDir, "alc.dll"),
                Path.Combine(_alExtDir, "bin", "win32", "alc.exe"),
                Path.Combine(_alExtDir, "bin", "alc.exe"),
            };
            alcPath = candidates.FirstOrDefault(File.Exists) ?? "";
        }

        if (string.IsNullOrEmpty(alcPath) || !File.Exists(alcPath))
        {
            throw new RpcException(-32000,
                $"alc compiler not found. Searched in: {_alExtDir}. " +
                "Pass alcPath in the request or ensure the AL extension directory contains alc.");
        }

        // Resolve package cache path
        var packageCachePath = "";
        if (@params.TryGetProperty("packageCachePath", out var pkgElem))
            packageCachePath = pkgElem.GetString() ?? "";
        if (@params.TryGetProperty("package_cache_path", out var pkgElem2))
            packageCachePath = pkgElem2.GetString() ?? "";
        if (string.IsNullOrEmpty(packageCachePath))
            packageCachePath = Path.Combine(project, ".alpackages");

        try
        {
            return RunAlcCompiler(alcPath, project, packageCachePath);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Compilation error: {ex}");
            return new
            {
                success = false,
                diagnostics = Array.Empty<object>(),
                appPath = (string?)null,
                error = ex.Message,
            };
        }
    }

    /// <summary>
    /// Run alc.exe with error logging and parse the results.
    /// </summary>
    private object RunAlcCompiler(string alcPath, string projectDir, string packageCachePath)
    {
        // Create a temp file for error log output
        var errorLogPath = Path.Combine(Path.GetTempPath(), $"al-errorlog-{Guid.NewGuid():N}.xml");

        try
        {
            // Build alc arguments
            var isAlcDll = alcPath.EndsWith(".dll", StringComparison.OrdinalIgnoreCase);
            var startInfo = new ProcessStartInfo
            {
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };

            if (isAlcDll)
            {
                // Run via dotnet
                startInfo.FileName = "dotnet";
                startInfo.Arguments = $"\"{alcPath}\" " +
                    $"/project:\"{projectDir}\" " +
                    $"/packagecachepath:\"{packageCachePath}\" " +
                    $"/errorlog:\"{errorLogPath}\"";
            }
            else
            {
                startInfo.FileName = alcPath;
                startInfo.Arguments =
                    $"/project:\"{projectDir}\" " +
                    $"/packagecachepath:\"{packageCachePath}\" " +
                    $"/errorlog:\"{errorLogPath}\"";
            }

            Console.Error.WriteLine($"  Running: {startInfo.FileName} {startInfo.Arguments}");

            using var process = Process.Start(startInfo);
            if (process == null)
            {
                throw new RpcException(-32000, "Failed to start alc compiler process");
            }

            var stdout = process.StandardOutput.ReadToEnd();
            var stderr = process.StandardError.ReadToEnd();
            process.WaitForExit(120000); // 2 minute timeout

            var exitCode = process.ExitCode;
            Console.Error.WriteLine($"  alc exit code: {exitCode}");

            if (!string.IsNullOrEmpty(stderr))
                Console.Error.WriteLine($"  alc stderr: {stderr}");

            // Parse diagnostics from error log or stdout
            var diagnostics = new List<object>();
            string? appPath = null;

            // Try to parse the error log XML
            if (File.Exists(errorLogPath))
            {
                diagnostics = ParseErrorLog(errorLogPath);
            }

            // Also parse stdout for any additional output
            if (diagnostics.Count == 0 && !string.IsNullOrWhiteSpace(stdout))
            {
                diagnostics = ParseAlcStdout(stdout, projectDir);
            }

            // Look for the generated .app file
            var appJsonPath = Path.Combine(projectDir, "app.json");
            if (File.Exists(appJsonPath))
            {
                try
                {
                    var appJson = JsonSerializer.Deserialize<JsonElement>(File.ReadAllText(appJsonPath));
                    var appName = appJson.GetProperty("name").GetString() ?? "app";
                    var appPublisher = appJson.GetProperty("publisher").GetString() ?? "publisher";
                    var appVersion = appJson.GetProperty("version").GetString() ?? "1.0.0.0";
                    var expectedApp = Path.Combine(projectDir, "output",
                        $"{appPublisher}_{appName}_{appVersion}.app");
                    if (File.Exists(expectedApp))
                        appPath = expectedApp;
                }
                catch { /* ignore app.json parse errors */ }
            }

            return new
            {
                success = exitCode == 0,
                diagnostics,
                appPath,
            };
        }
        finally
        {
            // Clean up temp error log
            try { if (File.Exists(errorLogPath)) File.Delete(errorLogPath); }
            catch { /* ignore */ }
        }
    }

    /// <summary>
    /// Parse SARIF/XML error log produced by alc /errorlog.
    /// The error log is typically in SARIF JSON format.
    /// </summary>
    private List<object> ParseErrorLog(string errorLogPath)
    {
        var results = new List<object>();

        try
        {
            var content = File.ReadAllText(errorLogPath);

            // alc produces SARIF JSON format
            if (content.TrimStart().StartsWith("{"))
            {
                var sarif = JsonSerializer.Deserialize<JsonElement>(content);

                // Navigate: $.runs[0].results[]
                if (sarif.TryGetProperty("runs", out var runs) && runs.GetArrayLength() > 0)
                {
                    var run = runs[0];
                    if (run.TryGetProperty("results", out var sarifResults))
                    {
                        foreach (var result in sarifResults.EnumerateArray())
                        {
                            try
                            {
                                var ruleId = result.TryGetProperty("ruleId", out var rId) ? rId.GetString() ?? "" : "";
                                var message = "";
                                if (result.TryGetProperty("message", out var msg) && msg.TryGetProperty("text", out var msgText))
                                    message = msgText.GetString() ?? "";

                                var level = result.TryGetProperty("level", out var lvl) ? lvl.GetString() ?? "warning" : "warning";
                                var severity = level switch
                                {
                                    "error" => "error",
                                    "warning" => "warning",
                                    "note" => "info",
                                    _ => "warning",
                                };

                                uint line = 0, column = 0, endLine = 0, endColumn = 0;
                                var file = "";

                                if (result.TryGetProperty("locations", out var locs) && locs.GetArrayLength() > 0)
                                {
                                    var loc = locs[0];
                                    if (loc.TryGetProperty("physicalLocation", out var physLoc))
                                    {
                                        if (physLoc.TryGetProperty("artifactLocation", out var artLoc) &&
                                            artLoc.TryGetProperty("uri", out var uri))
                                        {
                                            file = uri.GetString() ?? "";
                                            // Convert URI to path if needed
                                            if (file.StartsWith("file:///"))
                                                file = new Uri(file).LocalPath;
                                        }

                                        if (physLoc.TryGetProperty("region", out var region))
                                        {
                                            line = region.TryGetProperty("startLine", out var sl) ? (uint)sl.GetInt32() - 1 : 0;
                                            column = region.TryGetProperty("startColumn", out var sc) ? (uint)sc.GetInt32() - 1 : 0;
                                            endLine = region.TryGetProperty("endLine", out var el) ? (uint)el.GetInt32() - 1 : line;
                                            endColumn = region.TryGetProperty("endColumn", out var ec) ? (uint)ec.GetInt32() - 1 : column;
                                        }
                                    }
                                }

                                results.Add(new
                                {
                                    file,
                                    line,
                                    column,
                                    endLine,
                                    endColumn,
                                    severity,
                                    code = ruleId,
                                    message,
                                });
                            }
                            catch { /* skip malformed results */ }
                        }
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Error parsing error log: {ex.Message}");
        }

        return results;
    }

    /// <summary>
    /// Parse alc stdout for diagnostic messages.
    /// Format: filepath(line,col): severity code: message
    /// </summary>
    private List<object> ParseAlcStdout(string stdout, string projectDir)
    {
        var results = new List<object>();

        foreach (var rawLine in stdout.Split('\n', StringSplitOptions.RemoveEmptyEntries))
        {
            var line = rawLine.Trim();
            if (string.IsNullOrEmpty(line)) continue;

            // Pattern: path(line,col): severity AL####: message
            var parenIdx = line.IndexOf('(');
            var closeParen = line.IndexOf(')', parenIdx + 1);
            if (parenIdx < 0 || closeParen < 0) continue;

            var file = line[..parenIdx];
            var posStr = line[(parenIdx + 1)..closeParen];
            var rest = line[(closeParen + 1)..].TrimStart(':', ' ');

            // Parse position
            var posParts = posStr.Split(',');
            uint lineNum = 0, colNum = 0;
            if (posParts.Length >= 1) uint.TryParse(posParts[0], out lineNum);
            if (posParts.Length >= 2) uint.TryParse(posParts[1], out colNum);
            // Convert to 0-based
            if (lineNum > 0) lineNum--;
            if (colNum > 0) colNum--;

            // Parse severity and code
            var severity = "warning";
            var code = "";
            var message = rest;

            var colonIdx = rest.IndexOf(':');
            if (colonIdx > 0)
            {
                var prefix = rest[..colonIdx].Trim();
                message = rest[(colonIdx + 1)..].Trim();

                if (prefix.StartsWith("error", StringComparison.OrdinalIgnoreCase))
                {
                    severity = "error";
                    code = prefix.Length > 6 ? prefix[6..].Trim() : "";
                }
                else if (prefix.StartsWith("warning", StringComparison.OrdinalIgnoreCase))
                {
                    severity = "warning";
                    code = prefix.Length > 8 ? prefix[8..].Trim() : "";
                }
                else if (prefix.StartsWith("info", StringComparison.OrdinalIgnoreCase))
                {
                    severity = "info";
                    code = prefix.Length > 5 ? prefix[5..].Trim() : "";
                }
            }

            results.Add(new
            {
                file,
                line = lineNum,
                column = colNum,
                endLine = lineNum,
                endColumn = colNum,
                severity,
                code,
                message,
            });
        }

        return results;
    }

    // -----------------------------------------------------------------------
    // typeAt — resolve symbol type at a source position
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "typeAt" request. Parses the source file, locates the token
    /// at the given position, and attempts to extract type information.
    ///
    /// If a SemanticModel is available (via Compilation), it is used for full
    /// type resolution. Otherwise, syntactic information is returned.
    /// </summary>
    public object? HandleTypeAt(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var line = @params.GetProperty("line").GetUInt32();
        var column = @params.GetProperty("column").GetUInt32();

        try
        {
            return ResolveTypeAt(file, line, column);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"TypeAt error: {ex}");
            return null;
        }
    }

    private object? ResolveTypeAt(string file, uint line, uint column)
    {
        if (!File.Exists(file))
            return null;

        var source = File.ReadAllText(file);
        var tree = ParseSource(source, file);
        if (tree == null) return null;

        var treeType = tree.GetType();

        // Get the root syntax node
        var root = _getCompilationUnitRootMethod?.Invoke(tree, null);
        if (root == null) return null;

        // Convert line/column to an offset position
        int offset = LineColumnToOffset(source, (int)line, (int)column);
        if (offset < 0) return null;

        // Find the token at the position
        var rootType = root.GetType();
        var findTokenMethod = rootType.GetMethod("FindToken",
            BindingFlags.Instance | BindingFlags.Public);
        if (findTokenMethod == null) return null;

        object? token;
        var findTokenParams = findTokenMethod.GetParameters();
        if (findTokenParams.Length == 1 && findTokenParams[0].ParameterType == typeof(int))
        {
            token = findTokenMethod.Invoke(root, new object[] { offset });
        }
        else
        {
            // Try with additional params
            var args = new object?[findTokenParams.Length];
            args[0] = offset;
            for (int i = 1; i < findTokenParams.Length; i++)
            {
                if (findTokenParams[i].HasDefaultValue)
                    args[i] = findTokenParams[i].DefaultValue;
                else
                    args[i] = null;
            }
            token = findTokenMethod.Invoke(root, args);
        }

        if (token == null) return null;

        var tokenType = token.GetType();
        var tokenText = token.ToString() ?? "";
        var tokenKind = GetPropertyValue(token, tokenType, "Kind")?.ToString() ?? "Unknown";

        // Get the parent node for context
        var parentProp = tokenType.GetProperty("Parent");
        var parent = parentProp?.GetValue(token);
        var parentKind = "";
        if (parent != null)
        {
            parentKind = GetPropertyValue(parent, parent.GetType(), "Kind")?.ToString() ?? "";
        }

        // Try to create a Compilation and get semantic info
        string typeName = tokenText;
        string typeKind = parentKind;
        string? doc = null;

        if (_compilationType != null)
        {
            try
            {
                var compilation = CreateCompilation(new[] { tree }, "");
                if (compilation != null)
                {
                    // Try to get SemanticModel
                    var getSemanticModel = compilation.GetType().GetMethod("GetSemanticModel",
                        BindingFlags.Instance | BindingFlags.Public);
                    if (getSemanticModel != null)
                    {
                        var semanticModel = getSemanticModel.Invoke(compilation, new[] { tree });
                        if (semanticModel != null)
                        {
                            var (resolvedName, resolvedKind, resolvedDoc) =
                                ExtractSymbolFromSemanticModel(semanticModel, offset);
                            if (!string.IsNullOrEmpty(resolvedName))
                            {
                                typeName = resolvedName;
                                typeKind = resolvedKind;
                                doc = resolvedDoc;
                            }
                        }
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"  Semantic resolution failed (non-fatal): {ex.Message}");
            }
        }

        return new
        {
            name = typeName,
            kind = typeKind,
            documentation = doc,
        };
    }

    /// <summary>
    /// Extract symbol information from a SemanticModel at a given position.
    /// </summary>
    private (string name, string kind, string? doc) ExtractSymbolFromSemanticModel(
        object semanticModel, int position)
    {
        var smType = semanticModel.GetType();

        // Try GetSymbolInfo
        var getSymInfo = smType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "GetSymbolInfo" &&
                m.GetParameters().Length >= 1 &&
                m.GetParameters()[0].ParameterType == typeof(int));

        // Try GetDeclaredSymbol
        var getDeclSym = smType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "GetDeclaredSymbol");

        // Try GetTypeInfo
        var getTypeInfo = smType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "GetTypeInfo" &&
                m.GetParameters().Length >= 1);

        // Attempt in order of specificity
        if (getTypeInfo != null)
        {
            try
            {
                var pars = getTypeInfo.GetParameters();
                var args = new object?[pars.Length];
                args[0] = position;
                for (int i = 1; i < pars.Length; i++)
                {
                    if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
                    else args[i] = null;
                }

                var typeInfoResult = getTypeInfo.Invoke(semanticModel, args);
                if (typeInfoResult != null)
                {
                    var resultType = typeInfoResult.GetType();
                    var typeProp = resultType.GetProperty("Type");
                    var typeSymbol = typeProp?.GetValue(typeInfoResult);
                    if (typeSymbol != null)
                    {
                        var name = GetPropertyValue<string>(typeSymbol, typeSymbol.GetType(), "Name") ?? "";
                        var navKind = GetPropertyValue(typeSymbol, typeSymbol.GetType(), "NavTypeKind")?.ToString() ?? "Unknown";
                        return (name, navKind, null);
                    }
                }
            }
            catch { /* fall through */ }
        }

        return ("", "", null);
    }

    /// <summary>Convert line/column (0-based) to a character offset.</summary>
    private static int LineColumnToOffset(string source, int line, int column)
    {
        int currentLine = 0;
        int offset = 0;

        while (offset < source.Length && currentLine < line)
        {
            if (source[offset] == '\n')
                currentLine++;
            offset++;
        }

        if (currentLine != line)
            return -1;

        return Math.Min(offset + column, source.Length - 1);
    }

    // -----------------------------------------------------------------------
    // completions — get completion items at a position
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "completions" request. Returns completion items at the given position.
    ///
    /// Completions require a full SemanticModel from a Compilation. If available,
    /// completions are derived from the symbol table at the cursor position.
    /// Otherwise, returns an empty list.
    /// </summary>
    public object HandleCompletions(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var line = @params.GetProperty("line").GetUInt32();
        var column = @params.GetProperty("column").GetUInt32();

        try
        {
            if (!File.Exists(file))
                return Array.Empty<object>();

            var source = File.ReadAllText(file);
            var tree = ParseSource(source, file);
            if (tree == null)
                return Array.Empty<object>();

            // Try to get completions via SemanticModel
            if (_compilationType != null)
            {
                var compilation = CreateCompilation(new[] { tree }, "");
                if (compilation != null)
                {
                    var getSemanticModel = compilation.GetType().GetMethod("GetSemanticModel",
                        BindingFlags.Instance | BindingFlags.Public);
                    if (getSemanticModel != null)
                    {
                        var semanticModel = getSemanticModel.Invoke(compilation, new[] { tree });
                        if (semanticModel != null)
                        {
                            int offset = LineColumnToOffset(source, (int)line, (int)column);
                            if (offset >= 0)
                            {
                                return ExtractCompletions(semanticModel, offset);
                            }
                        }
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Completions error: {ex.Message}");
        }

        return Array.Empty<object>();
    }

    /// <summary>
    /// Extract completion items from a SemanticModel at a given position.
    /// </summary>
    private object ExtractCompletions(object semanticModel, int position)
    {
        var results = new List<object>();
        var smType = semanticModel.GetType();

        // Look for LookupSymbols or similar method
        var lookupMethod = smType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "LookupSymbols" || m.Name == "GetCompletionSymbols");

        if (lookupMethod == null)
            return results;

        try
        {
            var pars = lookupMethod.GetParameters();
            var args = new object?[pars.Length];
            args[0] = position;
            for (int i = 1; i < pars.Length; i++)
            {
                if (pars[i].HasDefaultValue)
                    args[i] = pars[i].DefaultValue;
                else
                    args[i] = null;
            }

            var symbols = lookupMethod.Invoke(semanticModel, args);
            if (symbols is System.Collections.IEnumerable enumerable)
            {
                foreach (var symbol in enumerable)
                {
                    if (symbol == null) continue;
                    var symType = symbol.GetType();
                    var name = GetPropertyValue<string>(symbol, symType, "Name") ?? "";
                    var kind = GetPropertyValue(symbol, symType, "Kind")?.ToString() ?? "Variable";

                    if (!string.IsNullOrEmpty(name))
                    {
                        results.Add(new
                        {
                            label = name,
                            kind = MapSymbolKindToCompletionKind(kind),
                            detail = (string?)null,
                            documentation = (string?)null,
                        });
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Completion lookup failed: {ex.Message}");
        }

        return results;
    }

    /// <summary>Map AL SymbolKind to a completion kind string.</summary>
    private static string MapSymbolKindToCompletionKind(string symbolKind)
    {
        return symbolKind.ToLowerInvariant() switch
        {
            "method" or "trigger" => "Method",
            "field" => "Field",
            "variable" or "local" or "global" => "Variable",
            "parameter" => "Variable",
            "table" or "page" or "codeunit" or "report" or "query" or "xmlport" => "Class",
            "enum" or "enumextension" => "Enum",
            "interface" => "Interface",
            "property" => "Property",
            "action" => "Function",
            _ => "Variable",
        };
    }

    // -----------------------------------------------------------------------
    // builtins — extract all built-in types and their methods
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "builtins" request. Extracts all built-in AL types and their
    /// methods from the CodeAnalysis assembly using reflection.
    ///
    /// Strategy:
    ///   1. Enumerate the NavTypeKind enum for all built-in type names
    ///   2. Find corresponding *TypeSymbol classes in the Symbols namespace
    ///   3. For each type, discover methods via IMethodSymbol collections
    ///   4. Extract method signatures including parameters, return types, and docs
    ///
    /// This uses patterns reverse-engineered from LinterCop and al-code-outline.
    /// </summary>
    public object HandleBuiltins()
    {
        var typesDict = new Dictionary<string, BuiltinTypeInfo>(StringComparer.OrdinalIgnoreCase);

        try
        {
            // Step 1: Get all NavTypeKind values
            if (_navTypeKindEnum != null && _navTypeKindEnum.IsEnum)
            {
                foreach (var name in Enum.GetNames(_navTypeKindEnum))
                {
                    if (name == "None" || name == "Unknown" || name == "DotNet" || name == "Void")
                        continue;

                    typesDict[name] = new BuiltinTypeInfo { Name = name };
                }
                Console.Error.WriteLine($"  NavTypeKind values: {typesDict.Count}");
            }

            // Step 2: Find type symbol classes and extract methods
            ExtractMethodsFromTypeSymbols(typesDict);

            // Step 3: Try to extract methods from a built-in type registry
            ExtractMethodsFromRegistry(typesDict);

            // Step 4: Try to extract methods using a dummy Compilation
            ExtractMethodsViaCompilation(typesDict);

            // Log summary
            var withMethods = typesDict.Values.Count(t => t.Methods.Count > 0);
            Console.Error.WriteLine($"  Total types: {typesDict.Count}, with methods: {withMethods}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error extracting builtins: {ex}");
        }

        // Convert to output format
        return typesDict.Values.Select(t => new
        {
            name = t.Name,
            methods = t.Methods.Select(m => new
            {
                name = m.Name,
                parameters = m.Parameters.Select(p => new
                {
                    name = p.Name,
                    typeName = p.TypeName,
                    isVar = p.IsVar,
                }).ToArray(),
                returnType = m.ReturnType,
                documentation = m.Documentation ?? "",
            }).ToArray(),
        }).ToArray();
    }

    /// <summary>
    /// Extract methods from *TypeSymbol classes in the Symbols namespace.
    /// These classes often have static methods or properties that expose built-in methods.
    /// </summary>
    private void ExtractMethodsFromTypeSymbols(Dictionary<string, BuiltinTypeInfo> typesDict)
    {
        try
        {
            // Find IMethodSymbol and related interfaces
            var iMethodSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.IMethodSymbol");
            var iTypeSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.ITypeSymbol");
            var iParameterSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.IParameterSymbol");

            // Find all TypeSymbol classes
            var symbolTypes = FindTypes("Microsoft.Dynamics.Nav.CodeAnalysis")
                .Where(t => t.Name.EndsWith("TypeSymbol") && !t.IsInterface)
                .ToArray();

            Console.Error.WriteLine($"  Found {symbolTypes.Length} TypeSymbol classes");

            foreach (var symType in symbolTypes)
            {
                try
                {
                    // Extract the type name from the class name
                    var typeName = symType.Name
                        .Replace("TypeSymbol", "")
                        .Replace("BuiltIn", "")
                        .Replace("Type", "");

                    if (string.IsNullOrEmpty(typeName)) continue;

                    // Look for "GetMembers" or "Methods" property/method
                    ExtractMethodsFromSymbolType(symType, typeName, typesDict, iMethodSymbol, iParameterSymbol);
                }
                catch { /* skip types that fail */ }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Error in ExtractMethodsFromTypeSymbols: {ex.Message}");
        }
    }

    /// <summary>
    /// Extract methods from a specific TypeSymbol class by looking for
    /// GetMembers(), Methods property, or static method collections.
    /// </summary>
    private void ExtractMethodsFromSymbolType(
        Type symType, string typeName,
        Dictionary<string, BuiltinTypeInfo> typesDict,
        Type? iMethodSymbol, Type? iParameterSymbol)
    {
        // Try to find static instances or singleton accessors
        var instanceProp = symType.GetProperty("Instance",
            BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic);
        var defaultProp = symType.GetProperty("Default",
            BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic);

        object? instance = null;
        try
        {
            instance = instanceProp?.GetValue(null) ?? defaultProp?.GetValue(null);
        }
        catch { /* some singletons may throw */ }

        if (instance == null)
        {
            // Try to look for static method/property collections
            var staticMethods = symType.GetMethods(BindingFlags.Static | BindingFlags.Public)
                .Where(m => iMethodSymbol != null && (
                    iMethodSymbol.IsAssignableFrom(m.ReturnType) ||
                    (m.ReturnType.IsGenericType && m.ReturnType.GetGenericArguments()
                        .Any(g => iMethodSymbol.IsAssignableFrom(g)))))
                .ToArray();

            if (staticMethods.Length > 0)
            {
                // These static methods return IMethodSymbol instances
                foreach (var sm in staticMethods)
                {
                    try
                    {
                        var result = sm.Invoke(null, sm.GetParameters().Length == 0 ? null :
                            sm.GetParameters().Select(p =>
                                p.HasDefaultValue ? p.DefaultValue :
                                p.ParameterType.IsValueType ? Activator.CreateInstance(p.ParameterType) :
                                (object?)null).ToArray());

                        if (result is System.Collections.IEnumerable enumMethods)
                        {
                            foreach (var method in enumMethods)
                            {
                                if (method == null) continue;
                                AddMethodToType(typeName, method, typesDict, iParameterSymbol);
                            }
                        }
                        else if (result != null && iMethodSymbol != null && iMethodSymbol.IsAssignableFrom(result.GetType()))
                        {
                            AddMethodToType(typeName, result, typesDict, iParameterSymbol);
                        }
                    }
                    catch { /* skip methods that throw */ }
                }
            }

            return;
        }

        // We have an instance — look for GetMembers()
        var getMembersMethod = instance.GetType().GetMethod("GetMembers",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);

        if (getMembersMethod != null)
        {
            try
            {
                var members = getMembersMethod.Invoke(instance, null);
                if (members is System.Collections.IEnumerable enumMembers)
                {
                    foreach (var member in enumMembers)
                    {
                        if (member == null) continue;
                        var memberKind = GetPropertyValue(member, member.GetType(), "Kind")?.ToString();
                        if (memberKind == "Method")
                        {
                            AddMethodToType(typeName, member, typesDict, iParameterSymbol);
                        }
                    }
                }
            }
            catch { /* skip */ }
        }

        // Also look for a "Methods" or "BuiltInMethods" property
        var methodsProp = instance.GetType().GetProperty("Methods",
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)
            ?? instance.GetType().GetProperty("BuiltInMethods",
                BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);

        if (methodsProp != null)
        {
            try
            {
                var methods = methodsProp.GetValue(instance);
                if (methods is System.Collections.IEnumerable enumMethods)
                {
                    foreach (var method in enumMethods)
                    {
                        if (method == null) continue;
                        AddMethodToType(typeName, method, typesDict, iParameterSymbol);
                    }
                }
            }
            catch { /* skip */ }
        }
    }

    /// <summary>
    /// Add a method (IMethodSymbol) to a type's method list.
    /// </summary>
    private void AddMethodToType(
        string typeName, object methodSymbol,
        Dictionary<string, BuiltinTypeInfo> typesDict,
        Type? iParameterSymbol)
    {
        var msType = methodSymbol.GetType();

        var name = GetPropertyValue<string>(methodSymbol, msType, "Name");
        if (string.IsNullOrEmpty(name)) return;

        // Get return type
        string? returnType = null;
        var returnValueProp = msType.GetProperty("ReturnValueSymbol",
            BindingFlags.Instance | BindingFlags.Public);
        if (returnValueProp != null)
        {
            var retVal = returnValueProp.GetValue(methodSymbol);
            if (retVal != null)
            {
                var retType = GetPropertyValue(retVal, retVal.GetType(), "ReturnType");
                if (retType != null)
                {
                    returnType = GetPropertyValue<string>(retType, retType.GetType(), "Name")
                        ?? GetPropertyValue(retType, retType.GetType(), "NavTypeKind")?.ToString();
                }
            }
        }

        // If no ReturnValueSymbol, try ReturnType directly
        if (returnType == null)
        {
            var retTypeProp = msType.GetProperty("ReturnType",
                BindingFlags.Instance | BindingFlags.Public);
            if (retTypeProp != null)
            {
                var retTypeObj = retTypeProp.GetValue(methodSymbol);
                if (retTypeObj != null)
                {
                    returnType = GetPropertyValue<string>(retTypeObj, retTypeObj.GetType(), "Name")
                        ?? retTypeObj.ToString();
                    if (returnType == "Void" || returnType == "None")
                        returnType = null;
                }
            }
        }

        // Get parameters
        var parameters = new List<MethodParamInfo>();
        var paramsProp = msType.GetProperty("Parameters",
            BindingFlags.Instance | BindingFlags.Public);
        if (paramsProp != null)
        {
            try
            {
                var paramsObj = paramsProp.GetValue(methodSymbol);
                if (paramsObj is System.Collections.IEnumerable enumParams)
                {
                    foreach (var param in enumParams)
                    {
                        if (param == null) continue;
                        var pType = param.GetType();

                        var paramName = GetPropertyValue<string>(param, pType, "Name") ?? "param";
                        var isVar = false;
                        try
                        {
                            isVar = GetPropertyValue<bool>(param, pType, "IsVar");
                        }
                        catch { /* default false */ }

                        // Get parameter type
                        var paramTypeName = "Variant";
                        var paramTypeProp = pType.GetProperty("ParameterType",
                            BindingFlags.Instance | BindingFlags.Public);
                        if (paramTypeProp != null)
                        {
                            var paramTypeObj = paramTypeProp.GetValue(param);
                            if (paramTypeObj != null)
                            {
                                paramTypeName = GetPropertyValue<string>(paramTypeObj, paramTypeObj.GetType(), "Name")
                                    ?? GetPropertyValue(paramTypeObj, paramTypeObj.GetType(), "NavTypeKind")?.ToString()
                                    ?? "Variant";
                            }
                        }

                        parameters.Add(new MethodParamInfo
                        {
                            Name = paramName,
                            TypeName = paramTypeName,
                            IsVar = isVar,
                        });
                    }
                }
            }
            catch { /* skip param extraction errors */ }
        }

        // Ensure the type exists in our dictionary
        if (!typesDict.ContainsKey(typeName))
        {
            typesDict[typeName] = new BuiltinTypeInfo { Name = typeName };
        }

        // Avoid duplicates
        var typeInfo = typesDict[typeName];
        if (!typeInfo.Methods.Any(m => m.Name == name && m.Parameters.Count == parameters.Count))
        {
            typeInfo.Methods.Add(new AlMethodInfo
            {
                Name = name,
                Parameters = parameters,
                ReturnType = returnType,
                Documentation = null,
            });
        }
    }

    /// <summary>
    /// Try to find a type registry/factory that produces built-in type instances.
    /// </summary>
    private void ExtractMethodsFromRegistry(Dictionary<string, BuiltinTypeInfo> typesDict)
    {
        try
        {
            // Look for BuiltInTypeFactory, TypeFactory, or similar
            var factoryTypes = new[]
            {
                "Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.BuiltInTypeFactory",
                "Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.TypeFactory",
                "Microsoft.Dynamics.Nav.CodeAnalysis.BuiltInTypes",
                "Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.BuiltInTypes",
            };

            var iParameterSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.IParameterSymbol");
            var iMethodSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.IMethodSymbol");

            foreach (var factoryName in factoryTypes)
            {
                var factoryType = FindType(factoryName);
                if (factoryType == null) continue;

                Console.Error.WriteLine($"  Found type factory: {factoryName}");

                // Look for methods that return type symbols
                var getTypeMethod = factoryType.GetMethods(BindingFlags.Static | BindingFlags.Public)
                    .Where(m => m.Name.StartsWith("Get") || m.Name.StartsWith("Create"))
                    .ToArray();

                foreach (var method in getTypeMethod)
                {
                    try
                    {
                        if (method.GetParameters().Length > 0) continue;
                        var result = method.Invoke(null, null);
                        if (result == null) continue;

                        // Check if it's a type symbol with methods
                        var getMembersM = result.GetType().GetMethod("GetMembers",
                            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                        if (getMembersM != null)
                        {
                            var typeName = method.Name.Replace("Get", "").Replace("Create", "").Replace("Type", "");
                            if (string.IsNullOrEmpty(typeName))
                                typeName = GetPropertyValue<string>(result, result.GetType(), "Name") ?? "Unknown";

                            var members = getMembersM.Invoke(result, null);
                            if (members is System.Collections.IEnumerable enumMembers)
                            {
                                foreach (var member in enumMembers)
                                {
                                    if (member == null) continue;
                                    var kind = GetPropertyValue(member, member.GetType(), "Kind")?.ToString();
                                    if (kind == "Method")
                                    {
                                        AddMethodToType(typeName, member, typesDict, iParameterSymbol);
                                    }
                                }
                            }
                        }
                    }
                    catch { /* skip */ }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Registry extraction error: {ex.Message}");
        }
    }

    /// <summary>
    /// Extract built-in methods by creating a minimal Compilation and inspecting
    /// the global scope's available methods for each built-in type.
    /// </summary>
    private void ExtractMethodsViaCompilation(Dictionary<string, BuiltinTypeInfo> typesDict)
    {
        if (_compilationType == null || _parseObjectTextMethod == null) return;

        try
        {
            var iParameterSymbol = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.IParameterSymbol");

            // Create a minimal AL file that uses a variety of types
            var minimalSource = @"codeunit 50000 ""TypeProbe""
{
    procedure Probe()
    var
        t: Text;
        i: Integer;
        d: Decimal;
        b: Boolean;
        dt: DateTime;
        da: Date;
        ti: Time;
        g: Guid;
        bi: BigInteger;
        c: Char;
        dur: Duration;
        rv: RecordId;
    begin
    end;
}";

            var tree = ParseSource(minimalSource, "TypeProbe.al");
            if (tree == null) return;

            var compilation = CreateCompilation(new[] { tree }, "");
            if (compilation == null) return;

            // Get the global namespace or module
            var compType = compilation.GetType();
            var globalNs = GetPropertyValue(compilation, compType, "GlobalNamespace")
                ?? GetPropertyValue(compilation, compType, "Assembly");

            if (globalNs == null) return;

            // Look for GetTypeByMetadataName or similar
            var getTypeMethod = compType.GetMethods(BindingFlags.Instance | BindingFlags.Public)
                .FirstOrDefault(m => m.Name == "GetTypeByNavTypeKind" ||
                                     m.Name == "GetBuiltInType" ||
                                     m.Name == "GetSpecialType");

            if (getTypeMethod != null && _navTypeKindEnum != null)
            {
                foreach (var kvp in typesDict)
                {
                    try
                    {
                        if (Enum.TryParse(_navTypeKindEnum, kvp.Key, true, out var kindValue))
                        {
                            var typeSymbol = getTypeMethod.Invoke(compilation, new[] { kindValue });
                            if (typeSymbol != null)
                            {
                                var getMembers = typeSymbol.GetType().GetMethod("GetMembers",
                                    BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                                if (getMembers != null)
                                {
                                    var members = getMembers.Invoke(typeSymbol, null);
                                    if (members is System.Collections.IEnumerable enumMembers)
                                    {
                                        foreach (var member in enumMembers)
                                        {
                                            if (member == null) continue;
                                            var kind = GetPropertyValue(member, member.GetType(), "Kind")?.ToString();
                                            if (kind == "Method")
                                            {
                                                AddMethodToType(kvp.Key, member, typesDict, iParameterSymbol);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    catch { /* skip individual types that fail */ }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"  Compilation-based extraction error: {ex.Message}");
        }
    }

    // -----------------------------------------------------------------------
    // errorCodes — extract all compiler error/warning codes
    // -----------------------------------------------------------------------

    /// <summary>
    /// Handle the "errorCodes" request. Extracts all known AL compiler diagnostic
    /// codes from the CodeAnalysis assembly.
    ///
    /// Strategy (from anzwdev/al-code-outline's CompilerCodeAnalyzersLibrary):
    ///   1. Find the ErrorCode enum
    ///   2. For each enum value, create a NavDiagnosticInfo(errorCode) instance
    ///   3. Read the Descriptor property to get the DiagnosticDescriptor
    ///   4. Extract Id, Title, DefaultSeverity, Category, Description
    /// </summary>
    public object HandleErrorCodes()
    {
        var results = new List<object>();

        try
        {
            // Primary strategy: use NavDiagnosticInfo to get full DiagnosticDescriptor
            // per error code (pattern from anzwdev's CompilerCodeAnalyzersLibrary)
            if (_errorCodeEnum != null && _navDiagnosticInfoType != null)
            {
                results = ExtractErrorCodesViaNavDiagnosticInfo();
                if (results.Count > 0)
                {
                    Console.Error.WriteLine($"  Error codes extracted via NavDiagnosticInfo: {results.Count}");
                    return results;
                }
            }

            // Fallback 1: Just enumerate the ErrorCode enum
            if (_errorCodeEnum != null && _errorCodeEnum.IsEnum)
            {
                results = ExtractErrorCodesFromEnum();
                Console.Error.WriteLine($"  Error codes extracted from enum: {results.Count}");
                if (results.Count > 0) return results;
            }

            // Fallback 2: Search for DiagnosticDescriptor static fields
            results = ExtractErrorCodesFromDescriptorFields();
            Console.Error.WriteLine($"  Error codes extracted from descriptor fields: {results.Count}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error extracting error codes: {ex}");
        }

        return results;
    }

    /// <summary>
    /// Extract error codes using NavDiagnosticInfo(ErrorCode) constructor.
    /// This is the most reliable approach, yielding full descriptor info.
    /// Pattern from: anzwdev/al-code-outline CompilerCodeAnalyzersLibrary.cs
    /// </summary>
    private List<object> ExtractErrorCodesViaNavDiagnosticInfo()
    {
        var results = new List<object>();

        if (_errorCodeEnum == null || _navDiagnosticInfoType == null) return results;

        // Find the constructor: NavDiagnosticInfo(ErrorCode)
        var ctor = _navDiagnosticInfoType.GetConstructor(
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
            null, new[] { _errorCodeEnum }, null);

        if (ctor == null)
        {
            Console.Error.WriteLine("  NavDiagnosticInfo constructor not found");
            return results;
        }

        // Find the Descriptor property
        var descriptorProp = _navDiagnosticInfoType.GetProperty("Descriptor",
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);

        if (descriptorProp == null)
        {
            Console.Error.WriteLine("  NavDiagnosticInfo.Descriptor property not found");
            return results;
        }

        var errorCodes = Enum.GetValues(_errorCodeEnum);
        var ctorArgs = new object[1];

        foreach (var errorCode in errorCodes)
        {
            try
            {
                ctorArgs[0] = errorCode;
                var navDiagInfo = ctor.Invoke(ctorArgs);
                if (navDiagInfo == null) continue;

                var descriptor = descriptorProp.GetValue(navDiagInfo);
                if (descriptor == null) continue;

                var descType = descriptor.GetType();
                var id = GetPropertyValue<string>(descriptor, descType, "Id") ?? "";
                var titleObj = GetPropertyValue(descriptor, descType, "Title");
                var title = titleObj?.ToString() ?? errorCode.ToString() ?? "";
                var defaultSeverity = GetPropertyValue(descriptor, descType, "DefaultSeverity")?.ToString() ?? "Warning";
                var category = GetPropertyValue<string>(descriptor, descType, "Category") ?? "";
                var descriptionObj = GetPropertyValue(descriptor, descType, "Description");
                var description = descriptionObj?.ToString() ?? "";

                if (string.IsNullOrEmpty(id)) continue;

                // Build a readable message from title and description
                var message = !string.IsNullOrEmpty(title) ? title : errorCode.ToString() ?? id;

                results.Add(new
                {
                    code = id,
                    message,
                    severity = NormalizeSeverity(defaultSeverity),
                    category,
                    description,
                });
            }
            catch
            {
                // Skip individual error codes that fail
            }
        }

        return results;
    }

    /// <summary>
    /// Fallback: extract error codes by enumerating the ErrorCode enum directly.
    /// Less information (no title/description), but always works.
    /// </summary>
    private List<object> ExtractErrorCodesFromEnum()
    {
        var results = new List<object>();

        if (_errorCodeEnum == null || !_errorCodeEnum.IsEnum) return results;

        foreach (var name in Enum.GetNames(_errorCodeEnum))
        {
            if (name == "None" || name == "Unknown") continue;

            var value = Enum.Parse(_errorCodeEnum, name);
            var numericValue = Convert.ToInt32(value);

            results.Add(new
            {
                code = $"AL{numericValue:D4}",
                message = name,
                severity = "error",
                category = "",
                description = "",
            });
        }

        return results;
    }

    /// <summary>
    /// Fallback: find DiagnosticDescriptor static fields across all types.
    /// </summary>
    private List<object> ExtractErrorCodesFromDescriptorFields()
    {
        var results = new List<object>();

        if (_diagnosticDescriptorType == null) return results;

        var allTypes = FindTypes("Microsoft.Dynamics.Nav.CodeAnalysis")
            .Where(t => t.Name.Contains("Error") || t.Name.Contains("Diagnostic") ||
                        t.Name.Contains("Rule") || t.Name.Contains("Warning"));

        foreach (var type in allTypes)
        {
            try
            {
                var descriptorFields = type.GetFields(
                    BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)
                    .Where(f => _diagnosticDescriptorType.IsAssignableFrom(f.FieldType));

                foreach (var field in descriptorFields)
                {
                    try
                    {
                        var descriptor = field.GetValue(null);
                        if (descriptor == null) continue;

                        var descType = descriptor.GetType();
                        var id = GetPropertyValue<string>(descriptor, descType, "Id") ?? field.Name;
                        var title = GetPropertyValue(descriptor, descType, "Title")?.ToString() ?? "";
                        var severity = GetPropertyValue(descriptor, descType, "DefaultSeverity")?.ToString() ?? "Warning";

                        results.Add(new
                        {
                            code = id,
                            message = title,
                            severity = NormalizeSeverity(severity),
                            category = "",
                            description = "",
                        });
                    }
                    catch { /* skip */ }
                }
            }
            catch { /* skip types that fail to reflect */ }
        }

        return results;
    }

    // -----------------------------------------------------------------------
    // Reflection helpers
    // -----------------------------------------------------------------------

    /// <summary>Get a property or method return value by name, cast to T.</summary>
    private static T? GetPropertyValue<T>(object obj, Type type, string name)
    {
        // Try property first
        var prop = type.GetProperty(name,
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
        if (prop != null)
        {
            try
            {
                var val = prop.GetValue(obj);
                if (val is T typed) return typed;
            }
            catch { /* ignore accessor failures */ }
        }

        // Try as a parameterless method (for GetMessage() etc.)
        var method = type.GetMethod(name,
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic,
            null, Type.EmptyTypes, null);
        if (method != null)
        {
            try
            {
                var val = method.Invoke(obj, null);
                if (val is T typed) return typed;
            }
            catch { /* ignore invocation failures */ }
        }

        return default;
    }

    /// <summary>Get a property or method return value by name, untyped.</summary>
    private static object? GetPropertyValue(object obj, Type type, string name)
    {
        var prop = type.GetProperty(name,
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
        if (prop != null)
        {
            try { return prop.GetValue(obj); }
            catch { /* ignore */ }
        }

        var method = type.GetMethod(name,
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic,
            null, Type.EmptyTypes, null);
        if (method != null)
        {
            try { return method.Invoke(obj, null); }
            catch { /* ignore */ }
        }

        return null;
    }
}

// -----------------------------------------------------------------------
// Internal data structures for builtins extraction
// -----------------------------------------------------------------------

/// <summary>Accumulated info about a built-in type during extraction.</summary>
class BuiltinTypeInfo
{
    public string Name { get; set; } = "";
    public List<AlMethodInfo> Methods { get; set; } = new();
}

/// <summary>Accumulated info about a method during extraction.</summary>
class AlMethodInfo
{
    public string Name { get; set; } = "";
    public List<MethodParamInfo> Parameters { get; set; } = new();
    public string? ReturnType { get; set; }
    public string? Documentation { get; set; }
}

/// <summary>Accumulated info about a method parameter during extraction.</summary>
class MethodParamInfo
{
    public string Name { get; set; } = "";
    public string TypeName { get; set; } = "Variant";
    public bool IsVar { get; set; }
}
