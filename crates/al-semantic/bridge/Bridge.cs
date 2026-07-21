// AlBridge — Thin .NET reflection wrapper for CodeAnalysis.dll
//
// Loaded in-process by Rust via netcorehost. NOT a standalone program.
// Provides [UnmanagedCallersOnly] entry points for JSON-in/JSON-out communication.
//
// Architecture: Rust (all logic) -> netcorehost -> this DLL -> CodeAnalysis.dll

using System.Collections.Immutable;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;
using System.Text.Json;

namespace AlBridge;

public static class Bridge
{
    private const int MaxRequestBytes = 32 * 1024 * 1024;
    private const int MaxResponseBytes = 256 * 1024 * 1024;
    private static readonly object InitLock = new();
    private static readonly object RequestLock = new();
    private static CodeAnalysisBridge? _bridge;
    private static string? _codeAnalysisPath;
    /// <summary>
    /// Captured exception message from the last failed <c>Init</c> attempt,
    /// returned through <c>HandleRequest</c> so the Rust side has something
    /// more informative than "code -2" to log. Reset to null on a successful
    /// init.
    /// </summary>
    private static string? _lastInitError;
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        // The response envelope must retain `result: null`; omitting it makes a
        // legitimate "no type at this position" response indistinguishable
        // from a malformed envelope with neither result nor error.
        DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.Never,
        WriteIndented = false,
    };

    /// <summary>
    /// Initialize: load CodeAnalysis.dll and cache types.
    /// Args: UTF-8 path to CodeAnalysis.dll
    /// Returns: 0 on success, negative on error.
    /// </summary>
    [UnmanagedCallersOnly]
    public static unsafe int Init(byte* pathPtr, int pathLen)
    {
        ResolveEventHandler? resolver = null;
        try
        {
            if (pathPtr == null || pathLen <= 0 || pathLen > MaxRequestBytes)
                throw new ArgumentOutOfRangeException(nameof(pathLen), "CodeAnalysis path length is invalid.");

            var path = new UTF8Encoding(false, true).GetString(pathPtr, pathLen);
            path = Path.GetFullPath(path);
            if (!File.Exists(path))
                throw new FileNotFoundException("CodeAnalysis assembly was not found.", path);

            lock (InitLock)
            {
                if (_bridge is not null)
                {
                    if (string.Equals(_codeAnalysisPath, path, StringComparison.Ordinal)) return 0;
                    throw new InvalidOperationException(
                        $"AlBridge is already initialized for '{_codeAnalysisPath}'. " +
                        "A process can host only one AL CodeAnalysis toolchain.");
                }

                var alExtDir = Path.GetDirectoryName(path)
                    ?? throw new InvalidOperationException("CodeAnalysis assembly has no parent directory.");

                resolver = (_, args) =>
                {
                    var name = new AssemblyName(args.Name);
                    var candidates = new[]
                    {
                        Path.Combine(alExtDir, name.Name + ".dll"),
                        Path.Combine(alExtDir, "..", "Analyzers", name.Name + ".dll"),
                    };
                    foreach (var candidate in candidates)
                    {
                        if (!File.Exists(candidate)) continue;
                        try { return Assembly.LoadFrom(candidate); }
                        catch { /* try the next probing location */ }
                    }
                    return null;
                };
                AppDomain.CurrentDomain.AssemblyResolve += resolver;

                var asm = Assembly.LoadFrom(path);
                var bridge = new CodeAnalysisBridge(asm, alExtDir);
                _bridge = bridge;
                _codeAnalysisPath = path;
                _lastInitError = null;
                return 0;
            }
        }
        catch (Exception ex)
        {
            if (resolver != null) AppDomain.CurrentDomain.AssemblyResolve -= resolver;
            // Capture the exception so HandleRequest can surface it back to
            // Rust on the next call. Previously the catch was bare and the
            // caller saw only "code -2", which made remote diagnosis of a
            // missing CodeAnalysis dependency essentially impossible.
            _lastInitError = ex.ToString();
            return -2;
        }
    }

    /// <summary>
    /// Handle a JSON request. Returns pointer to UTF-8 JSON response.
    /// Caller must free with FreeBuffer.
    /// </summary>
    [UnmanagedCallersOnly]
    public static unsafe byte* HandleRequest(byte* requestPtr, int requestLen, int* responseLen)
    {
        if (responseLen == null) return null;
        *responseLen = 0;
        byte[] responseBytes;
        try
        {
            if (requestPtr == null || requestLen <= 0 || requestLen > MaxRequestBytes)
                throw new ArgumentOutOfRangeException(nameof(requestLen), "Bridge request length is invalid.");
            var json = new UTF8Encoding(false, true).GetString(requestPtr, requestLen);
            lock (RequestLock)
            {
                using var doc = JsonDocument.Parse(json);
                if (doc.RootElement.ValueKind != JsonValueKind.Object)
                    throw new JsonException("Bridge request root must be an object.");
                var method = doc.RootElement.GetProperty("method").GetString() ?? "";
                if (string.IsNullOrWhiteSpace(method)) throw new JsonException("Bridge method is required.");
                doc.RootElement.TryGetProperty("params", out var prms);

                // Reject calls that arrive before a successful Init. Without this
                // explicit guard the `_bridge?.Handle*` calls below silently return
                // null and the caller sees `{ "result": null }` — indistinguishable
                // from "no results for this query". Surface the original init error
                // (if any) so the daemon can log a real reason.
                if (method != "ping" && _bridge is null)
                {
                    throw new InvalidOperationException(
                        _lastInitError is null
                            ? "AlBridge: Init was never called or has not completed."
                            : $"AlBridge: Init failed previously: {_lastInitError}");
                }

                object? result = method switch
                {
                    "ping" => new { status = "ok", initialized = _bridge is not null },
                    "analyze" => _bridge?.HandleAnalyze(prms),
                    "builtins" => _bridge?.HandleBuiltins(),
                    "typeAt" => _bridge?.HandleTypeAt(prms),
                    "completions" => _bridge?.HandleCompletions(prms),
                    "errorCodes" => _bridge?.HandleErrorCodes(),
                    _ => throw new Exception($"Unknown method: {method}"),
                };

                responseBytes = JsonSerializer.SerializeToUtf8Bytes(
                    new { result }, JsonOpts);
            }
        }
        catch (Exception ex)
        {
            // Unwrap reflection wrappers (TargetInvocationException) to the
            // innermost exception so Rust logs the REAL failure (type + message)
            // instead of the opaque "Exception has been thrown by the target of
            // an invocation."
            var root = ex;
            while (root.InnerException != null) root = root.InnerException;
            var message = ReferenceEquals(root, ex) ? ex.Message : $"{root.GetType().Name}: {root.Message}";
            responseBytes = JsonSerializer.SerializeToUtf8Bytes(
                new { error = new { code = -1, message } }, JsonOpts);
        }

        if (responseBytes.Length > MaxResponseBytes)
        {
            responseBytes = JsonSerializer.SerializeToUtf8Bytes(
                new { error = new { code = -1, message = $"Bridge response exceeded {MaxResponseBytes} bytes." } },
                JsonOpts);
        }

        return AllocateResponse(responseBytes, responseLen);
    }

    /// <summary>Return the detailed exception captured by the last failed Init.</summary>
    [UnmanagedCallersOnly]
    public static unsafe byte* GetLastError(int* responseLen)
    {
        if (responseLen == null) return null;
        *responseLen = 0;
        var bytes = Encoding.UTF8.GetBytes(_lastInitError ?? "Unknown bridge initialization failure.");
        return AllocateResponse(bytes, responseLen);
    }

    private static unsafe byte* AllocateResponse(byte[] responseBytes, int* responseLen)
    {
        byte* ptr = null;
        try
        {
            ptr = (byte*)Marshal.AllocCoTaskMem(responseBytes.Length);
            if (ptr == null) return null;
            Marshal.Copy(responseBytes, 0, (nint)ptr, responseBytes.Length);
            *responseLen = responseBytes.Length;
            return ptr;
        }
        catch
        {
            if (ptr != null) Marshal.FreeCoTaskMem((nint)ptr);
            *responseLen = 0;
            return null;
        }
    }

    [UnmanagedCallersOnly]
    public static unsafe void FreeBuffer(byte* ptr) => Marshal.FreeCoTaskMem((nint)ptr);
}

internal class CodeAnalysisBridge
{
    private readonly Assembly _asm;
    private readonly string _alExtDir;

    private readonly Type? _syntaxTreeType;
    private readonly Type? _sourceTextType;
    private readonly Type? _compilationType;
    private readonly Type? _compilationOptionsType;
    private readonly Type? _diagnosticAnalyzerType;
    private readonly Type? _navTypeKindEnum;
    private readonly Type? _errorCodeEnum;
    private readonly Type? _navDiagnosticInfoType;

    private readonly MethodInfo? _parseObjectTextMethod;
    private readonly MethodInfo? _sourceTextFromStringMethod;
    private readonly MethodInfo? _getCompilationUnitRootMethod;

    public CodeAnalysisBridge(Assembly asm, string alExtDir)
    {
        _asm = asm;
        _alExtDir = alExtDir;

        _syntaxTreeType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Syntax.SyntaxTree");
        _sourceTextType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Text.SourceText");
        _compilationType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Compilation");
        _compilationOptionsType = F("Microsoft.Dynamics.Nav.CodeAnalysis.CompilationOptions");
        _diagnosticAnalyzerType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.DiagnosticAnalyzer");
        _navTypeKindEnum = F("Microsoft.Dynamics.Nav.CodeAnalysis.NavTypeKind");
        _errorCodeEnum = F("Microsoft.Dynamics.Nav.CodeAnalysis.ErrorCode");
        _navDiagnosticInfoType = F("Microsoft.Dynamics.Nav.CodeAnalysis.NavDiagnosticInfo");

        _sourceTextFromStringMethod = ResolveSourceTextFrom();
        _parseObjectTextMethod = ResolveParseObjectText();
        _getCompilationUnitRootMethod = _syntaxTreeType?.GetMethod("GetCompilationUnitRoot",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?? _syntaxTreeType?.GetMethod("GetRoot",
                BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);

        var missing = new List<string>();
        if (_syntaxTreeType == null) missing.Add("SyntaxTree");
        if (_sourceTextType == null) missing.Add("SourceText");
        if (_compilationType == null) missing.Add("Compilation");
        if (_parseObjectTextMethod == null) missing.Add("SyntaxTree.ParseObjectText");
        if (_sourceTextFromStringMethod == null) missing.Add("SourceText.From(string)");
        if (_getCompilationUnitRootMethod == null) missing.Add("SyntaxTree.GetCompilationUnitRoot/GetRoot");
        if (missing.Count > 0)
            throw new MissingMemberException(
                $"The CodeAnalysis assembly is incompatible; missing required API(s): {string.Join(", ", missing)}");
    }

    private Type? F(string name) { try { return _asm.GetType(name); } catch { return null; } }

    private IEnumerable<Type> FT(string ns)
    {
        try { return _asm.GetTypes().Where(t => t.Namespace?.StartsWith(ns) == true); }
        catch (ReflectionTypeLoadException ex) { return ex.Types.Where(t => t?.Namespace?.StartsWith(ns) == true)!; }
    }

    public object? HandleAnalyze(JsonElement prms)
    {
        var file = prms.GetProperty("file").GetString() ?? "";
        var source = prms.GetProperty("source").GetString() ?? "";
        var analyzers = new List<string>();
        if (prms.TryGetProperty("analyzers", out var a) && a.ValueKind == JsonValueKind.Array)
            foreach (var item in a.EnumerateArray()) { var n = item.GetString(); if (n != null) analyzers.Add(n); }
        var pkgCache = "";
        if (prms.TryGetProperty("packageCache", out var p)) pkgCache = p.GetString() ?? "";
        if (string.IsNullOrEmpty(pkgCache) && prms.TryGetProperty("package_cache", out var p2)) pkgCache = p2.GetString() ?? "";

        var tree = ParseSource(source, file)
            ?? throw new InvalidOperationException("CodeAnalysis returned no syntax tree.");

        // Compilation diagnostics include syntax, binding, and type checking.
        // Returning only SyntaxTree.GetDiagnostics here would make this bridge
        // look healthy while silently omitting the compiler semantics it exists
        // to provide.
        var comp = CreateCompilation(new[] { tree }, pkgCache)
            ?? throw new InvalidOperationException("Could not create an AL compilation for semantic analysis.");
        var diags = ExtractDiagnostics(comp, file);

        if (analyzers.Count > 0) diags.AddRange(RunAnalyzers(comp, analyzers));
        return diags;
    }

    public object? HandleBuiltins()
    {
        if (_navTypeKindEnum?.IsEnum != true)
            throw new MissingMemberException("CodeAnalysis does not expose the NavTypeKind catalog.");
        var types = new Dictionary<string, BTypeInfo>(StringComparer.OrdinalIgnoreCase);

        if (_navTypeKindEnum?.IsEnum == true)
            foreach (var n in Enum.GetNames(_navTypeKindEnum))
                if (n != "None" && n != "Unknown" && n != "DotNet" && n != "Void")
                    types[n] = new BTypeInfo { Name = n };

        ExtractMethodsFromTypeSymbols(types);
        ExtractMethodsViaCompilation(types);

        return types.Values.Select(t => new
        {
            name = t.Name,
            methods = t.Methods.Select(m => new
            {
                name = m.Name,
                parameters = m.Params.Select(p => new { name = p.Name, typeName = p.TypeName, isVar = p.IsVar }).ToArray(),
                returnType = m.ReturnType,
                documentation = "",
            }).ToArray(),
            enumValues = t.EnumValues.ToArray(),
        }).ToArray();
    }

    public object? HandleTypeAt(JsonElement prms)
    {
        var file = prms.GetProperty("file").GetString() ?? "";
        var line = prms.GetProperty("line").GetUInt32();
        var col = prms.GetProperty("column").GetUInt32();
        var pkgCache = GetOptionalString(prms, "packageCache");

        // F-037: prefer caller-supplied unsaved text over disk so hover
        // reflects the editor buffer, not the last-saved file. Disk read is
        // the fallback for callers that don't have the buffer (analyze).
        string? source = null;
        if (prms.TryGetProperty("text", out var textProp) && textProp.ValueKind == JsonValueKind.String)
        {
            source = textProp.GetString();
        }
        if (source == null)
        {
            if (!File.Exists(file)) return null;
            source = File.ReadAllText(file);
        }
        var tree = ParseSource(source, file)
            ?? throw new InvalidOperationException("CodeAnalysis returned no syntax tree.");

        var root = _getCompilationUnitRootMethod?.Invoke(tree, null);
        if (root == null) return null;

        int offset = LineColToOffset(source, (int)line, (int)col);
        if (offset < 0 || offset == source.Length) return null;

        var findToken = root.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "FindToken")
            .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType == typeof(int))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault();
        if (findToken == null) return null;

        var fp = findToken.GetParameters();
        object? token = fp.Length == 1
            ? findToken.Invoke(root, new object[] { offset })
            : findToken.Invoke(root, MakeArgs(fp, offset));
        if (token == null) return null;

        var tt = token.GetType();

        var comp = CreateCompilation(new[] { tree }, pkgCache)
            ?? throw new InvalidOperationException("Could not create an AL compilation for type lookup.");
        var getSM = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetSemanticModel")
            .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType.IsAssignableFrom(tree.GetType()))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault();
        var sm = getSM?.Invoke(comp, MakeArgs(getSM.GetParameters(), tree))
            ?? throw new MissingMemberException("CodeAnalysis does not expose a semantic model.");
        var node = tt.GetProperty("Parent", BindingFlags.Instance | BindingFlags.Public)?.GetValue(token);
        var (typeName, typeKind, doc) = ExtractFromSemanticModel(sm, node);
        if (string.IsNullOrEmpty(typeName)) return null;
        return new { name = typeName, kind = typeKind, documentation = doc };
    }

    public object? HandleCompletions(JsonElement prms)
    {
        var file = prms.GetProperty("file").GetString() ?? "";
        var line = prms.GetProperty("line").GetUInt32();
        var col = prms.GetProperty("column").GetUInt32();
        var pkgCache = GetOptionalString(prms, "packageCache");

        // F-037: prefer caller-supplied unsaved text over disk; see
        // HandleTypeAt for the same fallback contract.
        string? source = null;
        if (prms.TryGetProperty("text", out var textProp) && textProp.ValueKind == JsonValueKind.String)
        {
            source = textProp.GetString();
        }
        if (source == null)
        {
            if (!File.Exists(file)) return Array.Empty<object>();
            source = File.ReadAllText(file);
        }
        int offset = LineColToOffset(source, (int)line, (int)col);
        if (offset < 0) return Array.Empty<object>();

        // A completion request commonly arrives immediately after a trailing
        // dot. Insert a synthetic missing-member name after the cursor so the
        // parser builds a complete member-access node and the semantic model
        // can still bind the receiver. Text before the cursor (and therefore
        // the requested UTF-16 offset) is unchanged.
        var analysisSource = source;
        if (offset > 0 && source[offset - 1] == '.')
            analysisSource = source.Insert(offset, "__AlBridgeCompletionProbe;");

        var lookupOffset = offset;
        var lineStart = offset == 0 ? 0 : source.LastIndexOf('\n', offset - 1);
        lineStart = lineStart < 0 ? 0 : lineStart + 1;
        var dot = offset > lineStart ? source.LastIndexOf('.', offset - 1, offset - lineStart) : -1;
        if (dot > lineStart)
        {
            var suffix = source.AsSpan(dot + 1, offset - dot - 1);
            if (suffix.IsEmpty || suffix.ToString().All(c => char.IsLetterOrDigit(c) || c is '_' or '"'))
                lookupOffset = dot;
        }

        var tree = ParseSource(analysisSource, file)
            ?? throw new InvalidOperationException("CodeAnalysis returned no syntax tree.");

        var comp = CreateCompilation(new[] { tree }, pkgCache)
            ?? throw new InvalidOperationException("Could not create an AL compilation for completion lookup.");

        var getSM = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetSemanticModel")
            .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType.IsAssignableFrom(tree.GetType()))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault();
        var sm = getSM?.Invoke(comp, MakeArgs(getSM.GetParameters(), tree));
        if (sm == null) return Array.Empty<object>();

        var root = _getCompilationUnitRootMethod?.Invoke(tree, null)
            ?? throw new MissingMemberException("CodeAnalysis returned no compilation-unit root.");

        return ExtractCompletions(comp, sm, root, lookupOffset);
    }

    public object? HandleErrorCodes()
    {
        var results = new List<object>();
        if (_errorCodeEnum?.IsEnum != true)
            throw new MissingMemberException("CodeAnalysis does not expose the ErrorCode catalog.");

        var ctor = _navDiagnosticInfoType?.GetConstructor(
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
            null, new[] { _errorCodeEnum }, null);
        var descProp = _navDiagnosticInfoType?.GetProperty("Descriptor",
            BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);

        if (ctor == null || descProp == null)
        {
            if (_errorCodeEnum.IsEnum)
                foreach (var n in Enum.GetNames(_errorCodeEnum))
                    if (n != "None" && n != "Unknown")
                    {
                        var v = Convert.ToInt32(Enum.Parse(_errorCodeEnum, n));
                        results.Add(new { code = $"AL{v:D4}", message = n, severity = "error", category = "", description = "" });
                    }
            return results;
        }

        foreach (var ec in Enum.GetValues(_errorCodeEnum))
        {
            try
            {
                var info = ctor.Invoke(new[] { ec });
                var desc = descProp.GetValue(info);
                if (desc == null) continue;
                var dt = desc.GetType();
                var id = Prop<string>(desc, dt, "Id") ?? "";
                if (string.IsNullOrEmpty(id)) continue;
                results.Add(new
                {
                    code = id,
                    message = Prop(desc, dt, "Title")?.ToString() ?? ec.ToString() ?? id,
                    severity = NormSev(Prop(desc, dt, "DefaultSeverity")?.ToString() ?? "Warning"),
                    category = Prop<string>(desc, dt, "Category") ?? "",
                    description = Prop(desc, dt, "Description")?.ToString() ?? "",
                });
            }
            catch { /* skip */ }
        }
        return results;
    }

    public object? HandleCompile(JsonElement prms)
    {
        var project = prms.GetProperty("project").GetString()
            ?? throw new Exception("Missing 'project' parameter");
        if (!Directory.Exists(project))
            throw new Exception($"Project directory not found: {project}");

        var alcPath = "";
        if (prms.TryGetProperty("alcPath", out var ae)) alcPath = ae.GetString() ?? "";
        if (prms.TryGetProperty("alc_path", out var ae2)) alcPath = ae2.GetString() ?? "";

        if (string.IsNullOrEmpty(alcPath))
        {
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
            throw new Exception($"alc compiler not found in {_alExtDir}");

        var pkgCache = "";
        if (prms.TryGetProperty("packageCachePath", out var pe)) pkgCache = pe.GetString() ?? "";
        if (prms.TryGetProperty("package_cache_path", out var pe2)) pkgCache = pe2.GetString() ?? "";
        if (string.IsNullOrEmpty(pkgCache)) pkgCache = Path.Combine(project, ".alpackages");

        var errorLogPath = Path.Combine(Path.GetTempPath(), $"al-errorlog-{Guid.NewGuid():N}.json");
        try
        {
            var isAlcDll = alcPath.EndsWith(".dll", StringComparison.OrdinalIgnoreCase);
            var startInfo = new System.Diagnostics.ProcessStartInfo
            {
                RedirectStandardOutput = true,
                RedirectStandardError = true,
                UseShellExecute = false,
                CreateNoWindow = true,
            };

            var alcArgs = $"/project:\"{project}\" /packagecachepath:\"{pkgCache}\" /errorlog:\"{errorLogPath}\"";
            if (isAlcDll)
            {
                startInfo.FileName = "dotnet";
                startInfo.Arguments = $"\"{alcPath}\" {alcArgs}";
                // Permit alc.dll to run when only a newer .NET runtime is installed.
                startInfo.EnvironmentVariables["DOTNET_ROLL_FORWARD"] = "Major";
            }
            else
            {
                startInfo.FileName = alcPath;
                startInfo.Arguments = alcArgs;
            }

            using var process = System.Diagnostics.Process.Start(startInfo);
            if (process == null) throw new Exception("Failed to start alc compiler process");

            // Read stderr on a background task: reading both pipes
            // sequentially can deadlock when the child fills the un-drained
            // pipe's kernel buffer.
            var stderrTask = process.StandardError.ReadToEndAsync();
            var stdout = process.StandardOutput.ReadToEnd();
            if (!process.WaitForExit(120000))
            {
                try { process.Kill(entireProcessTree: true); } catch { }
                throw new Exception("alc compiler timed out after 120s");
            }
            var stderr = stderrTask.GetAwaiter().GetResult();

            var diagnostics = new List<object>();
            string? appPath = null;

            if (File.Exists(errorLogPath))
                diagnostics = ParseSarifErrorLog(errorLogPath);

            if (diagnostics.Count == 0 && !string.IsNullOrWhiteSpace(stdout))
                diagnostics = ParseAlcStdout(stdout);

            var appJsonPath = Path.Combine(project, "app.json");
            if (File.Exists(appJsonPath))
            {
                var appJson = JsonSerializer.Deserialize<JsonElement>(File.ReadAllText(appJsonPath));
                if (appJson.TryGetProperty("name", out var nameProperty)
                    && appJson.TryGetProperty("publisher", out var publisherProperty)
                    && appJson.TryGetProperty("version", out var versionProperty))
                {
                    var name = nameProperty.GetString();
                    var publisher = publisherProperty.GetString();
                    var version = versionProperty.GetString();
                    if (string.IsNullOrWhiteSpace(name)
                        || string.IsNullOrWhiteSpace(publisher)
                        || string.IsNullOrWhiteSpace(version))
                    {
                        throw new InvalidDataException("app.json contains empty package identity fields");
                    }
                    var expected = Path.Combine(project, "output", $"{publisher}_{name}_{version}.app");
                    if (File.Exists(expected)) appPath = expected;
                }
            }

            // Retain capped compiler output when structured diagnostics are unavailable.
            var rawOutput = (stdout + (string.IsNullOrWhiteSpace(stderr) ? "" : "\n" + stderr)).Trim();
            if (rawOutput.Length > 64 * 1024) rawOutput = rawOutput.Substring(0, 64 * 1024) + "\n…(truncated)";

            return new { success = process.ExitCode == 0, diagnostics, appPath, output = rawOutput };
        }
        finally
        {
            try { if (File.Exists(errorLogPath)) File.Delete(errorLogPath); } catch { }
        }
    }

    private List<object> ParseSarifErrorLog(string path)
    {
        var results = new List<object>();
        try
        {
            var content = File.ReadAllText(path);
            if (!content.TrimStart().StartsWith("{")) return results;
            var sarif = JsonSerializer.Deserialize<JsonElement>(content);
            if (!sarif.TryGetProperty("runs", out var runs) || runs.GetArrayLength() == 0) return results;
            if (!runs[0].TryGetProperty("results", out var sarifResults)) return results;

            foreach (var r in sarifResults.EnumerateArray())
            {
                try
                {
                    var ruleId = r.TryGetProperty("ruleId", out var ri) ? ri.GetString() ?? "" : "";
                    var message = "";
                    if (r.TryGetProperty("message", out var msg) && msg.TryGetProperty("text", out var mt))
                        message = mt.GetString() ?? "";
                    var level = r.TryGetProperty("level", out var lv) ? lv.GetString() ?? "warning" : "warning";
                    var severity = NormSev(level == "note" ? "info" : level);

                    uint line = 0, col = 0, eLine = 0, eCol = 0;
                    var file = "";
                    if (r.TryGetProperty("locations", out var locs) && locs.GetArrayLength() > 0)
                    {
                        var loc = locs[0];
                        if (loc.TryGetProperty("physicalLocation", out var pl))
                        {
                            if (pl.TryGetProperty("artifactLocation", out var al) && al.TryGetProperty("uri", out var u))
                            {
                                file = u.GetString() ?? "";
                                if (file.StartsWith("file:///")) file = new Uri(file).LocalPath;
                            }
                            if (pl.TryGetProperty("region", out var rg))
                            {
                                line = rg.TryGetProperty("startLine", out var sl) ? (uint)sl.GetInt32() : 0;
                                col = rg.TryGetProperty("startColumn", out var sc) ? (uint)sc.GetInt32() : 0;
                                eLine = rg.TryGetProperty("endLine", out var el) ? (uint)el.GetInt32() : line;
                                eCol = rg.TryGetProperty("endColumn", out var ec) ? (uint)ec.GetInt32() : col;
                            }
                        }
                    }
                    results.Add(new { file, line, column = col, endLine = eLine, endColumn = eCol, severity, code = ruleId, message });
                }
                catch { }
            }
        }
        catch { }
        return results;
    }

    private static List<object> ParseAlcStdout(string stdout)
    {
        var results = new List<object>();
        foreach (var rawLine in stdout.Split('\n', StringSplitOptions.RemoveEmptyEntries))
        {
            var ln = rawLine.Trim();
            var p1 = ln.IndexOf('('); var p2 = ln.IndexOf(')', p1 + 1);
            if (p1 < 0 || p2 < 0) continue;
            var file = ln[..p1];
            var posStr = ln[(p1 + 1)..p2]; var rest = ln[(p2 + 1)..].TrimStart(':', ' ');
            // Only accept the `path(line,col)` diagnostic shape: the parens
            // content must be numeric. Without this check, banner lines like
            // "Microsoft (R) AL Compiler version ..." parsed as phantom
            // warnings ("Microsoft :0:0: warning : AL Compiler version ...").
            if (!posStr.Split(',').All(part => part.Trim().Length > 0 && part.Trim().All(char.IsDigit)))
                continue;
            var pp = posStr.Split(',');
            uint lineNum = 0, colNum = 0;
            if (pp.Length >= 1) uint.TryParse(pp[0], out lineNum);
            if (pp.Length >= 2) uint.TryParse(pp[1], out colNum);

            var severity = "warning"; var code = ""; var message = rest;
            var ci = rest.IndexOf(':');
            if (ci > 0)
            {
                var prefix = rest[..ci].Trim(); message = rest[(ci + 1)..].Trim();
                if (prefix.StartsWith("error", StringComparison.OrdinalIgnoreCase)) { severity = "error"; code = prefix.Length > 6 ? prefix[6..].Trim() : ""; }
                else if (prefix.StartsWith("warning", StringComparison.OrdinalIgnoreCase)) { severity = "warning"; code = prefix.Length > 8 ? prefix[8..].Trim() : ""; }
                else if (prefix.StartsWith("info", StringComparison.OrdinalIgnoreCase)) { severity = "info"; code = prefix.Length > 5 ? prefix[5..].Trim() : ""; }
            }
            results.Add(new { file, line = lineNum, column = colNum, endLine = lineNum, endColumn = colNum, severity, code, message });
        }
        return results;
    }

    private object? ParseSource(string source, string filePath)
    {
        if (_parseObjectTextMethod == null || _sourceTextFromStringMethod == null) return null;

        var fp = _sourceTextFromStringMethod.GetParameters();
        object? sourceText = fp.Length == 1
            ? _sourceTextFromStringMethod.Invoke(null, new object[] { source })
            : _sourceTextFromStringMethod.Invoke(null, MakeArgsStr(fp, source));
        if (sourceText == null) return null;

        var pp = _parseObjectTextMethod.GetParameters();
        var parseArgs = new object?[pp.Length];
        for (int i = 0; i < pp.Length; i++)
        {
            var pt = pp[i].ParameterType;
            if (pt == _sourceTextType || pt.IsAssignableFrom(sourceText.GetType())) parseArgs[i] = sourceText;
            else if (pt == typeof(string)) parseArgs[i] = filePath;
            else if (pp[i].HasDefaultValue) parseArgs[i] = pp[i].DefaultValue;
            else parseArgs[i] = null;
        }
        return _parseObjectTextMethod.Invoke(null, parseArgs);
    }

    private object? CreateCompilation(object[] trees, string pkgCache)
    {
        if (_compilationType == null) return null;

        var creates = _compilationType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "Create").OrderByDescending(m => m.GetParameters().Length).ToArray();

        foreach (var cm in creates)
        {
            try
            {
                var pars = cm.GetParameters();
                var args = new object?[pars.Length];
                for (int i = 0; i < pars.Length; i++)
                {
                    var pt = pars[i].ParameterType;
                    if (pt == typeof(string))
                        args[i] = pars[i].Name?.Contains("name") == true ? "AlAnalysis" : "0.0.0.0";
                    else if (pt == typeof(Guid) || pt == typeof(Guid?)) args[i] = Guid.Empty;
                    else if (pt == typeof(Version)) args[i] = new Version(0, 0, 0, 0);
                    else if (pt.IsGenericType && pt.GetGenericTypeDefinition() == typeof(IEnumerable<>))
                    {
                        var et = pt.GetGenericArguments()[0];
                        if (_syntaxTreeType != null && (et == _syntaxTreeType || (trees.Length > 0 && et.IsAssignableFrom(trees[0].GetType()))))
                        {
                            var arr = Array.CreateInstance(et, trees.Length);
                            for (int j = 0; j < trees.Length; j++) arr.SetValue(trees[j], j);
                            args[i] = arr;
                        }
                    }
                    else if (_compilationOptionsType != null && pt == _compilationOptionsType)
                        args[i] = CreateOptions();
                    else if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
                    else if (!pt.IsValueType) args[i] = null;
                    else args[i] = Activator.CreateInstance(pt);
                }
                var comp = cm.Invoke(null, args);
                if (comp != null && !string.IsNullOrEmpty(pkgCache) && Directory.Exists(pkgCache))
                    comp = TryAddRefs(comp, pkgCache);
                return comp;
            }
            catch { continue; }
        }
        return null;
    }

    private object? CreateOptions()
    {
        if (_compilationOptionsType == null) return null;
        foreach (var ctor in _compilationOptionsType.GetConstructors(
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance).OrderBy(c => c.GetParameters().Length))
        {
            try
            {
                var pars = ctor.GetParameters();
                var args = new object?[pars.Length];
                for (int i = 0; i < pars.Length; i++)
                {
                    if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
                    else if (pars[i].ParameterType.IsEnum) args[i] = Enum.GetValues(pars[i].ParameterType).GetValue(0);
                    else if (!pars[i].ParameterType.IsValueType) args[i] = null;
                    else args[i] = Activator.CreateInstance(pars[i].ParameterType);
                }
                return ctor.Invoke(args);
            }
            catch { continue; }
        }
        return null;
    }

    private object TryAddRefs(object comp, string pkgCache)
    {
        var loaderType = F("Microsoft.Dynamics.Nav.CodeAnalysis.CommandLine.LocalCacheSymbolReferenceLoader")
            ?? FT("Microsoft.Dynamics.Nav.CodeAnalysis")
                .FirstOrDefault(t => t.Name == "LocalCacheSymbolReferenceLoader")
            ?? throw new MissingMemberException("CodeAnalysis does not expose LocalCacheSymbolReferenceLoader.");

        object? loader = null;
        Exception? lastError = null;
        foreach (var ctor in loaderType.GetConstructors(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance))
        {
            try
            {
                var pars = ctor.GetParameters();
                var args = new object?[pars.Length];
                for (int i = 0; i < pars.Length; i++)
                {
                    var pt = pars[i].ParameterType;
                    if (pt.IsAssignableFrom(typeof(List<string>))) args[i] = new List<string> { pkgCache };
                    else if (pt == typeof(string[])) args[i] = new[] { pkgCache };
                    else if (pt == typeof(string)) args[i] = pkgCache;
                    else if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
                    else if (!pt.IsValueType) args[i] = null;
                    else args[i] = Activator.CreateInstance(pt);
                }
                loader = ctor.Invoke(args);
                if (loader != null) break;
            }
            catch (Exception ex) { lastError = ex; }
        }
        if (loader == null)
            throw new InvalidOperationException("No compatible symbol-reference loader constructor succeeded.", lastError);

        var withRef = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "WithReferenceLoader" && m.GetParameters().Length >= 1)
            .FirstOrDefault(m => m.GetParameters()[0].ParameterType.IsAssignableFrom(loader.GetType()))
            ?? throw new MissingMethodException("CodeAnalysis compilation has no compatible WithReferenceLoader method.");
        return withRef.Invoke(comp, MakeArgs(withRef.GetParameters(), loader))
            ?? throw new InvalidOperationException("WithReferenceLoader returned no compilation.");
    }

    private List<object> ExtractDiagnostics(object src, string defaultFile)
    {
        var results = new List<object>();
        var overloads = src.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetDiagnostics").ToArray();

        // Pick the overload whose parameters are ALL optional — on CodeAnalysis
        // v17 that is `GetDiagnostics(CancellationToken = default)`, returning
        // every syntactic diagnostic for the tree. The previous `FirstOrDefault`
        // picked `GetDiagnostics(SyntaxNode node)` (a REQUIRED param) and invoked
        // it with null, throwing ArgumentNullException on every file. Fallback:
        // feed the compilation-unit root to a single-`SyntaxNode` overload.
        var getDiag = overloads
            .Where(m => m.GetParameters().All(p => p.HasDefaultValue))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault();

        object? diagObj;
        if (getDiag != null)
        {
            var dp = getDiag.GetParameters();
            diagObj = getDiag.Invoke(src, dp.Select(p => p.DefaultValue).ToArray());
        }
        else
        {
            var nodeDiag = overloads.FirstOrDefault(m =>
                m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType.Name == "SyntaxNode");
            var root = _getCompilationUnitRootMethod?.Invoke(src, null);
            if (nodeDiag == null || root == null) return results;
            var dp = nodeDiag.GetParameters();
            var args = new object?[dp.Length];
            args[0] = root;
            for (int i = 1; i < dp.Length; i++) args[i] = dp[i].HasDefaultValue ? dp[i].DefaultValue : null;
            diagObj = nodeDiag.Invoke(src, args);
        }

        if (diagObj is not System.Collections.IEnumerable en) return results;
        foreach (var d in en)
        {
            if (d == null) continue;
            try { results.Add(ConvertDiag(d, defaultFile)); }
            catch { /* skip */ }
        }
        return results;
    }

    private object ConvertDiag(object d, string defaultFile)
    {
        var dt = d.GetType();
        var id = Prop<string>(d, dt, "Id") ?? "";
        // v17's Diagnostic exposes only GetMessage(IFormatProvider) — no
        // parameterless overload — so the old Type.EmptyTypes lookup found
        // nothing and every message came back empty. Invoke whatever GetMessage
        // exists, feeding InvariantCulture to an IFormatProvider/Culture param.
        var getMsg = dt.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetMessage").OrderByDescending(m => m.GetParameters().Length).FirstOrDefault();
        string msg = "";
        if (getMsg != null)
        {
            var mp = getMsg.GetParameters();
            var margs = mp.Select(p => p.ParameterType.Name.Contains("Format") || p.ParameterType.Name.Contains("Culture")
                ? (object?)System.Globalization.CultureInfo.InvariantCulture
                : (p.HasDefaultValue ? p.DefaultValue : null)).ToArray();
            try { msg = getMsg.Invoke(d, margs)?.ToString() ?? ""; } catch { /* fall through to Message */ }
        }
        if (string.IsNullOrEmpty(msg)) msg = Prop<string>(d, dt, "Message") ?? "";
        var sev = Prop(d, dt, "Severity")?.ToString() ?? "Warning";

        uint line = 0, col = 0, eLine = 0, eCol = 0;
        var file = defaultFile;
        var loc = Prop(d, dt, "Location");
        if (loc != null)
        {
            try
            {
                var lt = loc.GetType();
                var span = lt.GetMethod("GetLineSpan", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                    ?.Invoke(loc, null)
                    ?? lt.GetMethod("GetMappedLineSpan", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                        ?.Invoke(loc, null);
                if (span != null)
                {
                    var st = span.GetType();
                    var sp = st.GetProperty("Path")?.GetValue(span)?.ToString();
                    if (!string.IsNullOrEmpty(sp)) file = sp;
                    var start = st.GetProperty("StartLinePosition")?.GetValue(span);
                    if (start != null) { var pt = start.GetType(); line = (uint)Prop<int>(start, pt, "Line"); col = (uint)Prop<int>(start, pt, "Character"); }
                    var end = st.GetProperty("EndLinePosition")?.GetValue(span);
                    if (end != null) { var pt = end.GetType(); eLine = (uint)Prop<int>(end, pt, "Line"); eCol = (uint)Prop<int>(end, pt, "Character"); }
                }
            }
            catch { /* skip location */ }
        }

        return new { file, line, column = col, endLine = eLine, endColumn = eCol, severity = NormSev(sev), code = id, message = msg };
    }

    private List<object> RunAnalyzers(object comp, List<string> names)
    {
        var results = new List<object>();
        if (_diagnosticAnalyzerType == null)
            throw new MissingMemberException("CodeAnalysis does not expose DiagnosticAnalyzer.");

        foreach (var name in names)
        {
            var dllPath = ResolveAnalyzerPath(name)
                ?? throw new FileNotFoundException($"Requested analyzer '{name}' could not be resolved.");
            try
            {
                var aAsm = Assembly.LoadFrom(dllPath);
                var analyzerTypes = aAsm.GetTypes()
                    .Where(t => !t.IsAbstract && _diagnosticAnalyzerType.IsAssignableFrom(t))
                    .ToArray();
                if (analyzerTypes.Length == 0)
                    throw new InvalidOperationException($"'{dllPath}' contains no AL diagnostic analyzers.");
                foreach (var at in analyzerTypes)
                {
                    var analyzer = Activator.CreateInstance(at)
                        ?? throw new InvalidOperationException($"Could not construct analyzer '{at.FullName}'.");
                    results.AddRange(RunSingleAnalyzer(comp, analyzer));
                }
            }
            catch (Exception ex)
            {
                throw new InvalidOperationException($"Analyzer '{name}' failed: {ex.Message}", ex);
            }
        }
        return results;
    }

    private List<object> RunSingleAnalyzer(object comp, object analyzer)
    {
        var results = new List<object>();
        var cwaType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.CompilationWithAnalyzers");
        if (cwaType == null)
            throw new MissingMemberException("CodeAnalysis does not expose CompilationWithAnalyzers.");

        var immCreate = typeof(ImmutableArray).GetMethods(BindingFlags.Static | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "Create" && m.GetParameters().Length == 1 && m.GetParameters()[0].ParameterType.IsArray)
            ?? throw new MissingMemberException("ImmutableArray.Create<T>(T[]) was not found.");
        if (_diagnosticAnalyzerType == null)
            throw new MissingMemberException("CodeAnalysis does not expose DiagnosticAnalyzer.");

        var gc = immCreate.MakeGenericMethod(_diagnosticAnalyzerType);
        var arr = Array.CreateInstance(_diagnosticAnalyzerType, 1);
        arr.SetValue(analyzer, 0);
        var immAnalyzers = gc.Invoke(null, new object[] { arr });

        Exception? lastError = null;
        foreach (var ctor in cwaType.GetConstructors(BindingFlags.Public | BindingFlags.Instance))
        {
            try
            {
                var pars = ctor.GetParameters();
                var args = new object?[pars.Length];
                for (int i = 0; i < pars.Length; i++)
                {
                    if (pars[i].ParameterType == _compilationType) args[i] = comp;
                    else if (pars[i].ParameterType.Name.Contains("ImmutableArray")) args[i] = immAnalyzers;
                    else if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
                    else args[i] = null;
                }
                var cwa = ctor.Invoke(args);
                var getDiags = cwaType.GetMethod("GetAnalyzerDiagnosticsAsync",
                    BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                    ?? cwaType.GetMethod("GetAllDiagnosticsAsync",
                        BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                    ?? throw new MissingMethodException("No analyzer diagnostics method was found.");
                var task = getDiags.Invoke(cwa, null)
                    ?? throw new InvalidOperationException("Analyzer diagnostics returned no task.");
                task.GetType().GetMethod("Wait", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                    ?.Invoke(task, null);
                var diagResult = task.GetType().GetProperty("Result")?.GetValue(task);
                if (diagResult is not System.Collections.IEnumerable en)
                    throw new InvalidOperationException("Analyzer diagnostics returned an unexpected result.");
                foreach (var d in en)
                    if (d != null) results.Add(ConvertDiag(d, ""));
                return results;
            }
            catch (Exception ex) { lastError = ex; }
        }
        throw new InvalidOperationException(
            "No compatible CompilationWithAnalyzers constructor succeeded.", lastError);
    }

    private string? ResolveAnalyzerPath(string name)
    {
        if (File.Exists(name)) return name;
        var normalized = name.EndsWith(".dll", StringComparison.OrdinalIgnoreCase)
            ? name[..^4]
            : name;
        var productName = normalized.Equals("PerTenantCop", StringComparison.OrdinalIgnoreCase)
            ? "PerTenantExtensionCop"
            : normalized;
        var candidates = new[]
        {
            Path.Combine(_alExtDir, $"Microsoft.Dynamics.Nav.{productName}.dll"),
            Path.Combine(_alExtDir, $"Microsoft.Dynamics.Nav.Analyzers.{productName}.dll"),
            Path.Combine(_alExtDir, $"{productName}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"Microsoft.Dynamics.Nav.{productName}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"{productName}.dll"),
            Path.Combine(_alExtDir, "..", "Analyzers", $"Microsoft.Dynamics.Nav.{productName}.dll"),
            Path.Combine(_alExtDir, "..", "Analyzers", $"{productName}.dll"),
        };
        return candidates.Select(Path.GetFullPath).FirstOrDefault(File.Exists);
    }

    private void ExtractMethodsFromTypeSymbols(Dictionary<string, BTypeInfo> types)
    {
        var iMethodSym = F("Microsoft.Dynamics.Nav.CodeAnalysis.IMethodSymbol");
        var iParamSym = F("Microsoft.Dynamics.Nav.CodeAnalysis.IParameterSymbol");
        var symTypes = FT("Microsoft.Dynamics.Nav.CodeAnalysis").Where(t => t.Name.EndsWith("TypeSymbol") && !t.IsInterface).ToArray();

        foreach (var st in symTypes)
        {
            try
            {
                var tn = st.Name.Replace("TypeSymbol", "").Replace("BuiltIn", "").Replace("Type", "");
                if (string.IsNullOrEmpty(tn)) continue;

                var inst = st.GetProperty("Instance", BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)?.GetValue(null)
                    ?? st.GetProperty("Default", BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)?.GetValue(null);
                if (inst == null) continue;

                var getMembers = inst.GetType().GetMethod("GetMembers", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                if (getMembers?.Invoke(inst, null) is System.Collections.IEnumerable members)
                    foreach (var m in members) { if (m != null && Prop(m, m.GetType(), "Kind")?.ToString() == "Method") AddMethod(tn, m, types, iParamSym); }
            }
            catch { /* skip */ }
        }
    }

    private void ExtractMethodsViaCompilation(Dictionary<string, BTypeInfo> types)
    {
        if (_compilationType == null || _parseObjectTextMethod == null || _navTypeKindEnum == null) return;
        try
        {
            var tree = ParseSource("codeunit 50000 \"Probe\" { procedure P() var t: Text; begin end; }", "Probe.al");
            if (tree == null) return;
            var comp = CreateCompilation(new[] { tree }, "");
            if (comp == null) return;
            var iParamSym = F("Microsoft.Dynamics.Nav.CodeAnalysis.IParameterSymbol");

            var getType = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
                .FirstOrDefault(m => m.Name is "GetTypeByNavTypeKind" or "GetBuiltInType" or "GetSpecialType");
            if (getType == null) return;

            foreach (var kvp in types)
            {
                try
                {
                    if (!Enum.TryParse(_navTypeKindEnum, kvp.Key, true, out var kv)) continue;
                    var ts = getType.Invoke(comp, new[] { kv });
                    if (ts == null) continue;
                    var gm = ts.GetType().GetMethod("GetMembers", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                    if (gm?.Invoke(ts, null) is not System.Collections.IEnumerable members) continue;
                    foreach (var m in members)
                    {
                        if (m == null) continue;
                        var kind = Prop(m, m.GetType(), "Kind")?.ToString();
                        if (kind == "Method") AddMethod(kvp.Key, m, types, iParamSym);
                        else if (kind is "Field" or "EnumValue" or "EnumMember")
                        {
                            var mn = Prop(m, m.GetType(), "Name")?.ToString();
                            if (!string.IsNullOrEmpty(mn) && types.TryGetValue(kvp.Key, out var ti) && !ti.EnumValues.Contains(mn))
                                ti.EnumValues.Add(mn);
                        }
                    }
                }
                catch { /* skip */ }
            }
        }
        catch { /* skip */ }
    }

    private void AddMethod(string typeName, object ms, Dictionary<string, BTypeInfo> types, Type? iParamSym)
    {
        var mst = ms.GetType();
        var name = Prop<string>(ms, mst, "Name");
        if (string.IsNullOrEmpty(name)) return;

        string? retType = null;
        var rvp = mst.GetProperty("ReturnValueSymbol", BindingFlags.Instance | BindingFlags.Public);
        if (rvp?.GetValue(ms) is {} rv)
        {
            var rt = Prop(rv, rv.GetType(), "ReturnType");
            if (rt != null) retType = Prop<string>(rt, rt.GetType(), "Name") ?? Prop(rt, rt.GetType(), "NavTypeKind")?.ToString();
        }
        if (retType == null)
        {
            var rtp = mst.GetProperty("ReturnType", BindingFlags.Instance | BindingFlags.Public)?.GetValue(ms);
            if (rtp != null) { retType = Prop<string>(rtp, rtp.GetType(), "Name") ?? rtp.ToString(); if (retType is "Void" or "None") retType = null; }
        }

        var parms = new List<BParamInfo>();
        if (mst.GetProperty("Parameters", BindingFlags.Instance | BindingFlags.Public)?.GetValue(ms) is System.Collections.IEnumerable ep)
        {
            foreach (var p in ep)
            {
                if (p == null) continue;
                var pt = p.GetType();
                var ptn = "Variant";
                var ptp = pt.GetProperty("ParameterType", BindingFlags.Instance | BindingFlags.Public)?.GetValue(p);
                if (ptp != null) ptn = Prop<string>(ptp, ptp.GetType(), "Name") ?? Prop(ptp, ptp.GetType(), "NavTypeKind")?.ToString() ?? "Variant";
                var isVar = false; try { isVar = Prop<bool>(p, pt, "IsVar"); } catch { }
                parms.Add(new BParamInfo { Name = Prop<string>(p, pt, "Name") ?? "param", TypeName = ptn, IsVar = isVar });
            }
        }

        if (!types.ContainsKey(typeName)) types[typeName] = new BTypeInfo { Name = typeName };
        var ti = types[typeName];
        if (!ti.Methods.Any(m => m.Name == name && m.Params.Count == parms.Count))
            ti.Methods.Add(new BMethodInfo { Name = name, Params = parms, ReturnType = retType });
    }

    private object ExtractCompletions(object comp, object sm, object root, int pos)
    {
        var results = new List<object>();
        if (pos == 0) return results;
        var findToken = root.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "FindToken")
            .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType == typeof(int))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault()
            ?? throw new MissingMethodException("CodeAnalysis syntax root exposes no FindToken(int) method.");
        var tokenPos = pos - 1;
        var token = findToken.Invoke(root, MakeArgs(findToken.GetParameters(), tokenPos));
        if (token?.ToString() == "." && tokenPos > 0)
            token = findToken.Invoke(root, MakeArgs(findToken.GetParameters(), tokenPos - 1));
        var node = token == null ? null : Prop(token, "Parent");
        var type = FindSemanticType(sm, node);
        if (type == null)
            throw new InvalidOperationException(
                $"CodeAnalysis could not bind completion receiver token '{token}' ({Prop(node, "Kind")}).");

        var navKind = Prop(type, "NavTypeKind");
        if (navKind != null)
        {
            var getBuiltinType = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
                .Where(m => m.Name is "GetTypeByNavTypeKind" or "GetBuiltInType" or "GetSpecialType")
                .FirstOrDefault(m => m.GetParameters().Length >= 1
                    && m.GetParameters()[0].ParameterType.IsAssignableFrom(navKind.GetType()));
            type = getBuiltinType?.Invoke(comp, MakeArgs(getBuiltinType.GetParameters(), navKind)) ?? type;
        }

        var getMembers = type.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetMembers" && m.GetParameters().All(p => p.HasDefaultValue))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault();
        object? members = getMembers?.Invoke(type, getMembers.GetParameters().Select(p => p.DefaultValue).ToArray())
            ?? type.GetType().GetProperty("Members", BindingFlags.Instance | BindingFlags.Public)?.GetValue(type);
        if (members is not System.Collections.IEnumerable || !((System.Collections.IEnumerable)members).Cast<object?>().Any())
        {
            var getBinder = sm.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)
                .Where(m => m.Name is "GetEnclosingBinder" or "GetLookupBinder")
                .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType == typeof(int))
                .OrderBy(m => m.GetParameters().Length)
                .FirstOrDefault();
            var binder = getBinder?.Invoke(sm, MakeArgs(getBinder.GetParameters(), pos));
            var binderMemberLookup = binder?.GetType()
                .GetMethods(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic)
                .FirstOrDefault(m => m.Name == "GetMemberSymbolsFromType"
                    && m.GetParameters().Length == 1
                    && m.GetParameters()[0].ParameterType.IsAssignableFrom(type.GetType()));
            var binderMembers = binderMemberLookup?.Invoke(binder, new[] { type });
            if (binderMembers is System.Collections.IEnumerable binderEnumerable
                && binderEnumerable.Cast<object?>().Any())
                members = binderMembers;
        }
        if (members is not System.Collections.IEnumerable en)
            throw new MissingMemberException("Resolved CodeAnalysis type exposes no member collection.");
        foreach (var s in en)
        {
            if (s == null) continue;
            var st = s.GetType();
            var n = Prop<string>(s, st, "Name") ?? "";
            if (!string.IsNullOrEmpty(n))
                results.Add(new { label = n, kind = MapKind(Prop(s, st, "Kind")?.ToString() ?? ""), detail = (string?)null, documentation = (string?)null });
        }
        if (results.Count == 0)
            throw new InvalidOperationException(
                $"CodeAnalysis returned no members for type '{Prop<string>(type, type.GetType(), "Name")}' " +
                $"({type.GetType().FullName}, NavTypeKind={Prop(type, "NavTypeKind")}).");
        return results;
    }

    private (string, string, string?) ExtractFromSemanticModel(object sm, object? node)
    {
        var nestedType = FindSemanticType(sm, node);
        if (nestedType == null) return ("", "", null);
        var name = Prop<string>(nestedType, nestedType.GetType(), "Name") ?? "";
        var kind = Prop(nestedType, nestedType.GetType(), "NavTypeKind")?.ToString()
            ?? Prop(nestedType, nestedType.GetType(), "Kind")?.ToString()
            ?? "Unknown";
        return (name, kind, null);
    }

    private object? FindSemanticType(object sm, object? node)
    {
        if (node == null) return null;
        var smt = sm.GetType();
        var semanticMethods = smt.GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name is "GetTypeInfo" or "GetSymbolInfo")
            .ToArray();
        if (semanticMethods.Length == 0)
            throw new MissingMethodException("CodeAnalysis semantic model exposes no type or symbol lookup method.");
        for (var current = node; current != null; current = Prop(current, "Parent"))
        {
            foreach (var methodName in new[] { "GetTypeInfo", "GetSymbolInfo" })
            {
                var methods = semanticMethods
                    .Where(m => m.Name == methodName && m.GetParameters().Length >= 1)
                    .Where(m => m.GetParameters()[0].ParameterType.IsAssignableFrom(current.GetType()));
                foreach (var method in methods)
                {
                    try
                    {
                        var result = method.Invoke(sm, MakeArgs(method.GetParameters(), current));
                        if (result == null) continue;
                        var symbolOrType = Prop(result, "Type")
                            ?? Prop(result, "ConvertedType")
                            ?? Prop(result, "Symbol");
                        if (symbolOrType == null) continue;
                        var nestedType = Prop(symbolOrType, "Type")
                            ?? Prop(symbolOrType, "ReturnType")
                            ?? symbolOrType;
                        var name = Prop<string>(nestedType, nestedType.GetType(), "Name") ?? "";
                        if (!string.IsNullOrEmpty(name)) return nestedType;
                    }
                    catch { /* try another overload / ancestor node */ }
                }
            }
        }
        return null;
    }

    private MethodInfo? ResolveSourceTextFrom()
    {
        if (_sourceTextType == null) return null;
        var m = _sourceTextType.GetMethod("From", BindingFlags.Static | BindingFlags.Public, null, new[] { typeof(string) }, null);
        if (m != null) return m;
        return _sourceTextType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .FirstOrDefault(x => x.Name == "From" && x.GetParameters().Length >= 1 && x.GetParameters()[0].ParameterType == typeof(string));
    }

    private MethodInfo? ResolveParseObjectText()
    {
        var overloads = _syntaxTreeType?.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "ParseObjectText").ToArray() ?? Array.Empty<MethodInfo>();
        // Prefer the overload whose first parameter is SourceText so ParseSource
        // feeds it the SourceText we built. The String-first overload has TWO
        // string params (text, path); ParseSource's type-based arg filler can't
        // tell them apart and assigns the file PATH to both — parsing the path as
        // AL source and emitting phantom diagnostics at line 0.
        return overloads.FirstOrDefault(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType == _sourceTextType)
            ?? overloads.OrderBy(m => m.GetParameters().Length).FirstOrDefault();
    }

    private static int LineColToOffset(string s, int line, int col)
    {
        if (line < 0 || col < 0) return -1;

        int currentLine = 0;
        int lineStart = 0;
        while (currentLine < line)
        {
            int newline = s.IndexOf('\n', lineStart);
            if (newline < 0) return -1;
            lineStart = newline + 1;
            currentLine++;
        }

        int lineEnd = s.IndexOf('\n', lineStart);
        if (lineEnd < 0) lineEnd = s.Length;
        if (lineEnd > lineStart && s[lineEnd - 1] == '\r') lineEnd--;

        int lineLength = lineEnd - lineStart;
        if (col > lineLength) return -1;
        return lineStart + col;
    }

    private static object?[] MakeArgs(ParameterInfo[] pars, object? firstArg)
    {
        var args = new object?[pars.Length];
        args[0] = firstArg;
        for (int i = 1; i < pars.Length; i++) args[i] = pars[i].HasDefaultValue ? pars[i].DefaultValue : null;
        return args;
    }

    private static object?[] MakeArgsStr(ParameterInfo[] pars, string firstArg)
    {
        var args = new object?[pars.Length];
        args[0] = firstArg;
        for (int i = 1; i < pars.Length; i++)
        {
            if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
            else if (pars[i].ParameterType == typeof(Encoding)) args[i] = Encoding.UTF8;
            else args[i] = null;
        }
        return args;
    }

    private static string GetOptionalString(JsonElement prms, string name)
    {
        return prms.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String
            ? value.GetString() ?? ""
            : "";
    }

    private static T? Prop<T>(object obj, Type t, string name)
    {
        var p = t.GetProperty(name, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
        if (p != null) try { if (p.GetValue(obj) is T v) return v; } catch { }
        var m = t.GetMethod(name, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic, null, Type.EmptyTypes, null);
        if (m != null) try { if (m.Invoke(obj, null) is T v) return v; } catch { }
        return default;
    }

    private static object? Prop(object? obj, string name)
    {
        if (obj == null) return null;
        return Prop<object>(obj, obj.GetType(), name);
    }

    private static object? Prop(object obj, Type t, string name)
    {
        var p = t.GetProperty(name, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic);
        if (p != null) try { return p.GetValue(obj); } catch { }
        var m = t.GetMethod(name, BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic, null, Type.EmptyTypes, null);
        if (m != null) try { return m.Invoke(obj, null); } catch { }
        return null;
    }

    private static string NormSev(string s) => s.ToLowerInvariant() switch
    {
        "error" => "error", "warning" => "warning", "info" or "information" => "info", "hidden" or "hint" => "hint", _ => "warning",
    };

    private static string MapKind(string k) => k.ToLowerInvariant() switch
    {
        "method" or "trigger" => "Method", "field" => "Field", "variable" or "local" or "global" or "parameter" => "Variable",
        "table" or "page" or "codeunit" or "report" or "query" or "xmlport" => "Class",
        "enum" or "enumextension" => "Enum", "interface" => "Interface", "property" => "Property", _ => "Variable",
    };
}

internal class BTypeInfo { public string Name = ""; public List<BMethodInfo> Methods = new(); public List<string> EnumValues = new(); }
internal class BMethodInfo { public string Name = ""; public List<BParamInfo> Params = new(); public string? ReturnType; }
internal class BParamInfo { public string Name = ""; public string TypeName = "Variant"; public bool IsVar; }
