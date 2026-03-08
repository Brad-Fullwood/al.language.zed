// AlSemantic — .NET bridge for AL CodeAnalysis API
//
// Runs as a subprocess. Reads JSON-RPC requests from stdin, writes responses to stdout.
// Loads CodeAnalysis.dll from the AL Tool installation path (passed as first argument).
//
// Commands:
//   ping        — health check
//   analyze     — run DiagnosticAnalyzers on source
//   compile     — invoke Compilation API
//   typeAt      — resolve symbol at position
//   completions — get completion items at position
//   builtins    — extract all built-in types and methods
//   errorCodes  — list all compiler error codes
//   shutdown    — exit cleanly

using System.Reflection;
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

        var bridge = new CodeAnalysisBridge(codeAnalysis!);

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

    static object HandlePing()
    {
        return new { status = "ok" };
    }

    static object HandleShutdown()
    {
        // Schedule exit after responding
        _ = Task.Run(async () =>
        {
            await Task.Delay(100);
            Environment.Exit(0);
        });
        return new { status = "shutting_down" };
    }
}

/// Custom exception for RPC errors with error codes.
class RpcException : Exception
{
    public int Code { get; }

    public RpcException(int code, string message) : base(message)
    {
        Code = code;
    }
}

/// Bridge to CodeAnalysis functionality via reflection.
class CodeAnalysisBridge
{
    private readonly Assembly _codeAnalysis;

    // Cached types from CodeAnalysis
    private readonly Type? _diagnosticDescriptorType;
    private readonly Type? _diagnosticSeverityType;

    public CodeAnalysisBridge(Assembly codeAnalysis)
    {
        _codeAnalysis = codeAnalysis;

        // Try to find key types from the assembly
        _diagnosticDescriptorType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.DiagnosticDescriptor");
        _diagnosticSeverityType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.DiagnosticSeverity");
    }

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
                .Where(t => t.IsPublic && t.Namespace?.StartsWith(namespacePrefix) == true);
        }
        catch (ReflectionTypeLoadException ex)
        {
            // Some types may fail to load — return the ones that succeeded
            return ex.Types.Where(t => t != null && t.IsPublic &&
                t.Namespace?.StartsWith(namespacePrefix) == true)!;
        }
    }

    // -----------------------------------------------------------------------
    // analyze
    // -----------------------------------------------------------------------

    public object HandleAnalyze(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var source = @params.GetProperty("source").GetString()
            ?? throw new RpcException(-32602, "Missing 'source' parameter");

        var analyzers = new List<string>();
        if (@params.TryGetProperty("analyzers", out var analyzersElem))
        {
            foreach (var item in analyzersElem.EnumerateArray())
            {
                var name = item.GetString();
                if (name != null) analyzers.Add(name);
            }
        }

        // Try to use the Compilation API for analysis
        try
        {
            return RunAnalyzers(file, source, analyzers);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Analyzer error: {ex.Message}");
            // Return empty diagnostics rather than failing
            return Array.Empty<object>();
        }
    }

    private object RunAnalyzers(string file, string source, List<string> analyzers)
    {
        // Use reflection to create a SyntaxTree and run analyzers
        var syntaxTreeType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.SyntaxTree");
        if (syntaxTreeType == null)
        {
            throw new RpcException(-32000, "SyntaxTree type not found in CodeAnalysis");
        }

        // Try ParseObjectText or similar static method
        var parseMethod = syntaxTreeType.GetMethod("ParseObjectText",
            BindingFlags.Static | BindingFlags.Public,
            null, new[] { typeof(string), typeof(string) }, null);

        if (parseMethod == null)
        {
            // Try other parsing methods
            parseMethod = syntaxTreeType.GetMethod("ParseObjectText",
                BindingFlags.Static | BindingFlags.Public);
        }

        if (parseMethod == null)
        {
            throw new RpcException(-32000, "Could not find ParseObjectText method on SyntaxTree");
        }

        var tree = parseMethod.Invoke(null, new object[] { source, file });
        if (tree == null)
        {
            return Array.Empty<object>();
        }

        // Get diagnostics from the syntax tree
        var getDiagnosticsMethod = syntaxTreeType.GetMethod("GetDiagnostics",
            BindingFlags.Instance | BindingFlags.Public);

        if (getDiagnosticsMethod == null)
        {
            return Array.Empty<object>();
        }

        var diagnosticsObj = getDiagnosticsMethod.Invoke(tree, null);
        return ConvertDiagnostics(diagnosticsObj, file);
    }

    private object ConvertDiagnostics(object? diagnosticsObj, string file)
    {
        if (diagnosticsObj == null) return Array.Empty<object>();

        var results = new List<object>();

        if (diagnosticsObj is System.Collections.IEnumerable enumerable)
        {
            foreach (var diag in enumerable)
            {
                if (diag == null) continue;
                var diagType = diag.GetType();

                var id = GetPropertyValue<string>(diag, diagType, "Id") ?? "";
                var message = GetPropertyValue<string>(diag, diagType, "GetMessage") ??
                    GetPropertyValue<string>(diag, diagType, "Message") ?? "";
                var severity = GetPropertyValue(diag, diagType, "Severity")?.ToString() ?? "Warning";

                // Try to get location
                var location = GetPropertyValue(diag, diagType, "Location");
                uint line = 0, column = 0, endLine = 0, endColumn = 0;

                if (location != null)
                {
                    try
                    {
                        var locType = location.GetType();
                        var span = GetPropertyValue(location, locType, "GetLineSpan")
                            ?? GetPropertyValue(location, locType, "GetMappedLineSpan");

                        if (span != null)
                        {
                            var spanType = span.GetType();
                            var startLineSpan = GetPropertyValue(span, spanType, "StartLinePosition");
                            var endLineSpan = GetPropertyValue(span, spanType, "EndLinePosition");

                            if (startLineSpan != null)
                            {
                                var slType = startLineSpan.GetType();
                                line = (uint)(GetPropertyValue<int>(startLineSpan, slType, "Line"));
                                column = (uint)(GetPropertyValue<int>(startLineSpan, slType, "Character"));
                            }
                            if (endLineSpan != null)
                            {
                                var elType = endLineSpan.GetType();
                                endLine = (uint)(GetPropertyValue<int>(endLineSpan, elType, "Line"));
                                endColumn = (uint)(GetPropertyValue<int>(endLineSpan, elType, "Character"));
                            }
                        }
                    }
                    catch
                    {
                        // Location extraction failed — use defaults
                    }
                }

                results.Add(new
                {
                    file,
                    line,
                    column,
                    endLine,
                    endColumn,
                    severity = severity.ToLower(),
                    code = id,
                    message,
                });
            }
        }

        return results;
    }

    // -----------------------------------------------------------------------
    // compile
    // -----------------------------------------------------------------------

    public object HandleCompile(JsonElement @params)
    {
        var project = @params.GetProperty("project").GetString()
            ?? throw new RpcException(-32602, "Missing 'project' parameter");

        if (!Directory.Exists(project))
        {
            throw new RpcException(-32001, $"Project directory not found: {project}");
        }

        // Compilation would require creating a full Compilation object via reflection.
        // For now, report that compilation requires the alc.exe compiler.
        // This is a placeholder for when we integrate the full Compilation API.
        try
        {
            return RunCompilation(project);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Compilation error: {ex.Message}");
            return new
            {
                success = false,
                diagnostics = Array.Empty<object>(),
                appPath = (string?)null,
            };
        }
    }

    private object RunCompilation(string projectDir)
    {
        // Try to find and use the Compilation type
        var compilationType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Compilation");

        if (compilationType == null)
        {
            throw new RpcException(-32000, "Compilation type not found in CodeAnalysis");
        }

        // For a proper implementation, we would:
        // 1. Create a Compilation from the project's source files
        // 2. Add references from .alpackages
        // 3. Call Emit() to produce the .app output
        // 4. Collect diagnostics
        //
        // This is a complex operation that depends on many CodeAnalysis internals.
        // Return a not-yet-implemented response.
        throw new RpcException(-32000,
            "Full compilation via CodeAnalysis API is not yet implemented. Use alc.exe for compilation.");
    }

    // -----------------------------------------------------------------------
    // typeAt
    // -----------------------------------------------------------------------

    public object? HandleTypeAt(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var line = @params.GetProperty("line").GetUInt32();
        var column = @params.GetProperty("column").GetUInt32();

        // Type resolution requires a SemanticModel which requires a Compilation.
        // For now, we can try basic symbol resolution from a syntax tree.
        try
        {
            return ResolveTypeAt(file, line, column);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"TypeAt error: {ex.Message}");
            return null;
        }
    }

    private object? ResolveTypeAt(string file, uint line, uint column)
    {
        if (!File.Exists(file))
        {
            return null;
        }

        var source = File.ReadAllText(file);
        var syntaxTreeType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.SyntaxTree");
        if (syntaxTreeType == null) return null;

        var parseMethod = syntaxTreeType.GetMethod("ParseObjectText",
            BindingFlags.Static | BindingFlags.Public);
        if (parseMethod == null) return null;

        var tree = parseMethod.Invoke(null, new object[] { source, file });
        if (tree == null) return null;

        // Get the root node and find the token at the position
        var getRootMethod = syntaxTreeType.GetMethod("GetRoot",
            BindingFlags.Instance | BindingFlags.Public);
        if (getRootMethod == null) return null;

        var root = getRootMethod.Invoke(tree, null);
        if (root == null) return null;

        // Try to find a node at the position and extract type info
        var rootType = root.GetType();
        var findTokenMethod = rootType.GetMethod("FindToken",
            BindingFlags.Instance | BindingFlags.Public);

        if (findTokenMethod == null) return null;

        // We need to convert line/column to an offset
        // This is a simplified approach
        var getText = syntaxTreeType.GetMethod("GetText",
            BindingFlags.Instance | BindingFlags.Public);
        if (getText == null) return null;

        var text = getText.Invoke(tree, null);
        if (text == null) return null;

        // Return null for now — full type resolution requires SemanticModel
        return null;
    }

    // -----------------------------------------------------------------------
    // completions
    // -----------------------------------------------------------------------

    public object HandleCompletions(JsonElement @params)
    {
        var file = @params.GetProperty("file").GetString()
            ?? throw new RpcException(-32602, "Missing 'file' parameter");
        var line = @params.GetProperty("line").GetUInt32();
        var column = @params.GetProperty("column").GetUInt32();

        // Completions require a SemanticModel. Return empty for now.
        // Future: build a Compilation, get SemanticModel, use CompletionService.
        return Array.Empty<object>();
    }

    // -----------------------------------------------------------------------
    // builtins
    // -----------------------------------------------------------------------

    public object HandleBuiltins()
    {
        var results = new List<object>();

        try
        {
            // Look for the built-in type definitions in CodeAnalysis
            // The types are typically in the Microsoft.Dynamics.Nav.CodeAnalysis.Symbols namespace
            var symbolTypes = FindTypes("Microsoft.Dynamics.Nav.CodeAnalysis");

            // Look for types that represent built-in AL types
            // Common patterns: IBuiltInType, BuiltInTypeSymbol, NavTypeSymbol
            var builtInInterface = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.ITypeSymbol");
            var typeKindEnum = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.NavTypeKind")
                ?? FindType("Microsoft.Dynamics.Nav.CodeAnalysis.NavTypeKind");

            // Try to find a type registry or factory
            var typeFactoryType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.BuiltInTypeFactory")
                ?? FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.TypeFactory");

            if (typeKindEnum != null && typeKindEnum.IsEnum)
            {
                // Extract enum values as built-in type names
                foreach (var name in Enum.GetNames(typeKindEnum))
                {
                    if (name == "None" || name == "Unknown") continue;

                    results.Add(new
                    {
                        name,
                        methods = Array.Empty<object>(),
                    });
                }
            }

            // Also look for method definitions on built-in types
            ExtractBuiltInMethods(results);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error extracting builtins: {ex.Message}");
        }

        return results;
    }

    private void ExtractBuiltInMethods(List<object> results)
    {
        try
        {
            // Look for BuiltInMethodSymbol or similar types
            var methodSymbolType = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.BuiltInMethodSymbol")
                ?? FindType("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols.IMethodSymbol");

            if (methodSymbolType == null) return;

            // Look for static method collections on built-in type classes
            var builtInTypes = FindTypes("Microsoft.Dynamics.Nav.CodeAnalysis.Symbols")
                .Where(t => t.Name.EndsWith("TypeSymbol") || t.Name.EndsWith("BuiltInType"));

            foreach (var builtInType in builtInTypes)
            {
                var methods = new List<object>();
                var memberMethods = builtInType.GetMethods(BindingFlags.Static | BindingFlags.Public)
                    .Where(m => m.ReturnType == methodSymbolType ||
                                (methodSymbolType.IsInterface && methodSymbolType.IsAssignableFrom(m.ReturnType)));

                var typeName = builtInType.Name.Replace("TypeSymbol", "").Replace("BuiltInType", "");
                if (string.IsNullOrEmpty(typeName)) continue;

                // Check if we already have this type in results
                var existing = results.FindIndex(r =>
                {
                    var elem = JsonSerializer.SerializeToElement(r);
                    return elem.GetProperty("name").GetString() == typeName;
                });

                if (existing >= 0)
                {
                    // Update with methods if found
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error extracting built-in methods: {ex.Message}");
        }
    }

    // -----------------------------------------------------------------------
    // errorCodes
    // -----------------------------------------------------------------------

    public object HandleErrorCodes()
    {
        var results = new List<object>();

        try
        {
            // Look for DiagnosticDescriptor fields in CodeAnalysis
            // They're typically static readonly fields on error classes
            var errorTypes = FindTypes("Microsoft.Dynamics.Nav.CodeAnalysis")
                .Where(t => t.Name.Contains("Error") || t.Name.Contains("Diagnostic") ||
                            t.Name.Contains("Rule"));

            foreach (var errorType in errorTypes)
            {
                // Look for DiagnosticDescriptor fields
                var descriptorFields = errorType.GetFields(BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)
                    .Where(f => _diagnosticDescriptorType != null && f.FieldType == _diagnosticDescriptorType);

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
                            severity = severity.ToLower(),
                        });
                    }
                    catch
                    {
                        // Skip fields that can't be read
                    }
                }
            }

            // Also try to find error code enums
            var errorCodeEnum = FindType("Microsoft.Dynamics.Nav.CodeAnalysis.DiagnosticId")
                ?? FindType("Microsoft.Dynamics.Nav.CodeAnalysis.ErrorCode");

            if (errorCodeEnum != null && errorCodeEnum.IsEnum)
            {
                foreach (var name in Enum.GetNames(errorCodeEnum))
                {
                    if (name == "None" || name == "Unknown") continue;

                    var value = Enum.Parse(errorCodeEnum, name);
                    var numericValue = Convert.ToInt32(value);

                    // Only include if not already in results
                    var code = $"AL{numericValue:D4}";
                    if (!results.Any(r =>
                    {
                        var elem = JsonSerializer.SerializeToElement(r);
                        return elem.GetProperty("code").GetString() == code;
                    }))
                    {
                        results.Add(new
                        {
                            code,
                            message = name,
                            severity = "error",
                        });
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Error extracting error codes: {ex.Message}");
        }

        return results;
    }

    // -----------------------------------------------------------------------
    // Reflection helpers
    // -----------------------------------------------------------------------

    private static T? GetPropertyValue<T>(object obj, Type type, string name)
    {
        var prop = type.GetProperty(name, BindingFlags.Instance | BindingFlags.Public);
        if (prop != null)
        {
            var val = prop.GetValue(obj);
            if (val is T typed) return typed;
        }

        // Try as a method (for GetMessage() etc.)
        var method = type.GetMethod(name, BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
        if (method != null)
        {
            var val = method.Invoke(obj, null);
            if (val is T typed) return typed;
        }

        return default;
    }

    private static object? GetPropertyValue(object obj, Type type, string name)
    {
        var prop = type.GetProperty(name, BindingFlags.Instance | BindingFlags.Public);
        if (prop != null) return prop.GetValue(obj);

        var method = type.GetMethod(name, BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
        if (method != null) return method.Invoke(obj, null);

        return null;
    }
}
