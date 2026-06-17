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
    private static CodeAnalysisBridge? _bridge;
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
        DefaultIgnoreCondition = System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull,
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
        try
        {
            var path = Encoding.UTF8.GetString(pathPtr, pathLen);
            if (!File.Exists(path)) return -1;

            var alExtDir = Path.GetDirectoryName(path) ?? ".";

            AppDomain.CurrentDomain.AssemblyResolve += (_, args) =>
            {
                var name = new AssemblyName(args.Name);
                var candidate = Path.Combine(alExtDir, name.Name + ".dll");
                if (File.Exists(candidate))
                {
                    try { return Assembly.LoadFrom(candidate); }
                    catch { /* fall through */ }
                }
                return null;
            };

            var asm = Assembly.LoadFrom(path);
            _bridge = new CodeAnalysisBridge(asm, alExtDir);
            _lastInitError = null;
            return 0;
        }
        catch (Exception ex)
        {
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
        byte[] responseBytes;
        try
        {
            var json = Encoding.UTF8.GetString(requestPtr, requestLen);
            var doc = JsonDocument.Parse(json);
            var method = doc.RootElement.GetProperty("method").GetString() ?? "";
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
                "compile" => _bridge?.HandleCompile(prms),
                _ => throw new Exception($"Unknown method: {method}"),
            };

            responseBytes = JsonSerializer.SerializeToUtf8Bytes(
                new { result }, JsonOpts);
        }
        catch (Exception ex)
        {
            responseBytes = JsonSerializer.SerializeToUtf8Bytes(
                new { error = new { code = -1, message = ex.Message } }, JsonOpts);
        }

        var ptr = (byte*)Marshal.AllocCoTaskMem(responseBytes.Length);
        Marshal.Copy(responseBytes, 0, (nint)ptr, responseBytes.Length);
        *responseLen = responseBytes.Length;
        return ptr;
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

        var tree = ParseSource(source, file);
        if (tree == null) return Array.Empty<object>();

        var diags = ExtractDiagnostics(tree, file);

        if (analyzers.Count > 0 && _compilationType != null)
        {
            try
            {
                var comp = CreateCompilation(new[] { tree }, pkgCache);
                if (comp != null) diags.AddRange(RunAnalyzers(comp, analyzers));
            }
            catch { /* syntax diags still returned */ }
        }
        return diags;
    }

    public object? HandleBuiltins()
    {
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
                documentation = m.Doc ?? "",
            }).ToArray(),
            enumValues = t.EnumValues.ToArray(),
        }).ToArray();
    }

    public object? HandleTypeAt(JsonElement prms)
    {
        var file = prms.GetProperty("file").GetString() ?? "";
        var line = prms.GetProperty("line").GetUInt32();
        var col = prms.GetProperty("column").GetUInt32();

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
        var tree = ParseSource(source, file);
        if (tree == null) return null;

        var root = _getCompilationUnitRootMethod?.Invoke(tree, null);
        if (root == null) return null;

        int offset = LineColToOffset(source, (int)line, (int)col);
        if (offset < 0) return null;

        var findToken = root.GetType().GetMethod("FindToken", BindingFlags.Instance | BindingFlags.Public);
        if (findToken == null) return null;

        var fp = findToken.GetParameters();
        object? token = fp.Length == 1
            ? findToken.Invoke(root, new object[] { offset })
            : findToken.Invoke(root, MakeArgs(fp, offset));
        if (token == null) return null;

        var tt = token.GetType();
        var tokenText = token.ToString() ?? "";
        var parentKind = Prop(tt.GetProperty("Parent")?.GetValue(token), "Kind")?.ToString() ?? "";

        string typeName = tokenText, typeKind = parentKind;
        string? doc = null;

        if (_compilationType != null)
        {
            try
            {
                var comp = CreateCompilation(new[] { tree }, "");
                if (comp != null)
                {
                    var getSM = comp.GetType().GetMethod("GetSemanticModel", BindingFlags.Instance | BindingFlags.Public);
                    var sm = getSM?.Invoke(comp, new[] { tree });
                    if (sm != null)
                    {
                        var (rn, rk, _) = ExtractFromSemanticModel(sm, offset);
                        if (!string.IsNullOrEmpty(rn)) { typeName = rn; typeKind = rk; }
                    }
                }
            }
            catch { /* fall back to syntactic info */ }
        }

        return new { name = typeName, kind = typeKind, documentation = doc };
    }

    public object? HandleCompletions(JsonElement prms)
    {
        var file = prms.GetProperty("file").GetString() ?? "";
        var line = prms.GetProperty("line").GetUInt32();
        var col = prms.GetProperty("column").GetUInt32();

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
        var tree = ParseSource(source, file);
        if (tree == null) return Array.Empty<object>();

        if (_compilationType == null) return Array.Empty<object>();
        var comp = CreateCompilation(new[] { tree }, "");
        if (comp == null) return Array.Empty<object>();

        var getSM = comp.GetType().GetMethod("GetSemanticModel", BindingFlags.Instance | BindingFlags.Public);
        var sm = getSM?.Invoke(comp, new[] { tree });
        if (sm == null) return Array.Empty<object>();

        int offset = LineColToOffset(source, (int)line, (int)col);
        if (offset < 0) return Array.Empty<object>();

        return ExtractCompletions(sm, offset);
    }

    public object? HandleErrorCodes()
    {
        var results = new List<object>();
        if (_errorCodeEnum == null || _navDiagnosticInfoType == null) return results;

        var ctor = _navDiagnosticInfoType.GetConstructor(
            BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance,
            null, new[] { _errorCodeEnum }, null);
        var descProp = _navDiagnosticInfoType.GetProperty("Descriptor",
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
                // alc.dll targets net8.0; machines that only have a newer
                // runtime installed refuse to launch it without roll-forward.
                // The Rust-side alc fallback sets this too (toolchain.rs) —
                // without it the child died with "You must install or update
                // .NET" on stderr and this handler reported success=false
                // with ZERO diagnostics (FB-14).
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

            try
            {
                var appJsonPath = Path.Combine(project, "app.json");
                if (File.Exists(appJsonPath))
                {
                    var appJson = JsonSerializer.Deserialize<JsonElement>(File.ReadAllText(appJsonPath));
                    var name = appJson.GetProperty("name").GetString() ?? "app";
                    var publisher = appJson.GetProperty("publisher").GetString() ?? "publisher";
                    var version = appJson.GetProperty("version").GetString() ?? "1.0.0.0";
                    var expected = Path.Combine(project, "output", $"{publisher}_{name}_{version}.app");
                    if (File.Exists(expected)) appPath = expected;
                }
            }
            catch { /* ignore */ }

            // Always include raw compiler output (capped) so a failure can
            // never be silent — a non-zero exit with no parsed diagnostics
            // must still tell the user WHY (FB-14).
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
        try
        {
            var loaderType = F("Microsoft.Dynamics.Nav.CodeAnalysis.CommandLine.LocalCacheSymbolReferenceLoader");
            if (loaderType == null) return comp;

            object? loader = null;
            foreach (var ctor in loaderType.GetConstructors(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance))
            {
                try
                {
                    var pars = ctor.GetParameters();
                    var args = new object?[pars.Length];
                    args[0] = new List<string> { pkgCache };
                    for (int i = 1; i < pars.Length; i++) args[i] = pars[i].HasDefaultValue ? pars[i].DefaultValue : null;
                    loader = ctor.Invoke(args);
                    break;
                }
                catch { continue; }
            }
            if (loader == null) return comp;

            var withRef = comp.GetType().GetMethod("WithReferenceLoader", BindingFlags.Instance | BindingFlags.Public);
            return withRef?.Invoke(comp, new[] { loader }) ?? comp;
        }
        catch { return comp; }
    }

    private List<object> ExtractDiagnostics(object src, string defaultFile)
    {
        var results = new List<object>();
        var getDiag = src.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "GetDiagnostics");
        if (getDiag == null) return results;

        var dp = getDiag.GetParameters();
        var diagObj = dp.Length == 0
            ? getDiag.Invoke(src, null)
            : getDiag.Invoke(src, dp.Select(p => p.HasDefaultValue ? p.DefaultValue : (object?)null).ToArray());

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
        var msg = dt.GetMethod("GetMessage", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?.Invoke(d, null)?.ToString() ?? Prop<string>(d, dt, "Message") ?? "";
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
        if (_diagnosticAnalyzerType == null) return results;

        foreach (var name in names)
        {
            var dllPath = ResolveAnalyzerPath(name);
            if (dllPath == null) continue;
            try
            {
                var aAsm = Assembly.LoadFrom(dllPath);
                foreach (var at in aAsm.GetTypes().Where(t => !t.IsAbstract && _diagnosticAnalyzerType.IsAssignableFrom(t)))
                {
                    try
                    {
                        var analyzer = Activator.CreateInstance(at);
                        if (analyzer == null) continue;
                        results.AddRange(RunSingleAnalyzer(comp, analyzer));
                    }
                    catch { /* skip */ }
                }
            }
            catch { /* skip */ }
        }
        return results;
    }

    private List<object> RunSingleAnalyzer(object comp, object analyzer)
    {
        var results = new List<object>();
        var cwaType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.CompilationWithAnalyzers");
        if (cwaType == null) { results.AddRange(ExtractDiagnostics(comp, "")); return results; }

        try
        {
            var immCreate = typeof(ImmutableArray).GetMethods(BindingFlags.Static | BindingFlags.Public)
                .FirstOrDefault(m => m.Name == "Create" && m.GetParameters().Length == 1 && m.GetParameters()[0].ParameterType.IsArray);
            if (immCreate == null || _diagnosticAnalyzerType == null) return results;

            var gc = immCreate.MakeGenericMethod(_diagnosticAnalyzerType);
            var arr = Array.CreateInstance(_diagnosticAnalyzerType, 1);
            arr.SetValue(analyzer, 0);
            var immAnalyzers = gc.Invoke(null, new object[] { arr });

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
                            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null);
                    if (getDiags != null)
                    {
                        var task = getDiags.Invoke(cwa, null);
                        task?.GetType().GetMethod("Wait", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)?.Invoke(task, null);
                        var diagResult = task?.GetType().GetProperty("Result")?.GetValue(task);
                        if (diagResult is System.Collections.IEnumerable en)
                            foreach (var d in en) { if (d != null) try { results.Add(ConvertDiag(d, "")); } catch { } }
                    }
                    return results;
                }
                catch { continue; }
            }
        }
        catch { /* fallback */ }

        results.AddRange(ExtractDiagnostics(comp, ""));
        return results;
    }

    private string? ResolveAnalyzerPath(string name)
    {
        if (File.Exists(name)) return name;
        var candidates = new[]
        {
            Path.Combine(_alExtDir, $"Microsoft.Dynamics.Nav.Analyzers.{name}.dll"),
            Path.Combine(_alExtDir, $"{name}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"Microsoft.Dynamics.Nav.Analyzers.{name}.dll"),
            Path.Combine(_alExtDir, "Analyzers", $"{name}.dll"),
        };
        return candidates.FirstOrDefault(File.Exists);
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

    private object ExtractCompletions(object sm, int pos)
    {
        var results = new List<object>();
        var lookup = sm.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .FirstOrDefault(m => m.Name is "LookupSymbols" or "GetCompletionSymbols");
        if (lookup == null) return results;

        try
        {
            var syms = lookup.Invoke(sm, MakeArgs(lookup.GetParameters(), pos));
            if (syms is System.Collections.IEnumerable en)
                foreach (var s in en)
                {
                    if (s == null) continue;
                    var st = s.GetType();
                    var n = Prop<string>(s, st, "Name") ?? "";
                    if (!string.IsNullOrEmpty(n))
                        results.Add(new { label = n, kind = MapKind(Prop(s, st, "Kind")?.ToString() ?? ""), detail = (string?)null, documentation = (string?)null });
                }
        }
        catch { /* skip */ }
        return results;
    }

    private (string, string, string?) ExtractFromSemanticModel(object sm, int pos)
    {
        var smt = sm.GetType();
        var gti = smt.GetMethods(BindingFlags.Instance | BindingFlags.Public).FirstOrDefault(m => m.Name == "GetTypeInfo" && m.GetParameters().Length >= 1);
        if (gti != null)
        {
            try
            {
                var r = gti.Invoke(sm, MakeArgs(gti.GetParameters(), pos));
                if (r != null)
                {
                    var ts = r.GetType().GetProperty("Type")?.GetValue(r);
                    if (ts != null)
                    {
                        var n = Prop<string>(ts, ts.GetType(), "Name") ?? "";
                        var k = Prop(ts, ts.GetType(), "NavTypeKind")?.ToString() ?? "Unknown";
                        return (n, k, null);
                    }
                }
            }
            catch { /* fall through */ }
        }
        return ("", "", null);
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
        return _syntaxTreeType?.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .Where(m => m.Name == "ParseObjectText").OrderBy(m => m.GetParameters().Length).FirstOrDefault();
    }

    private static int LineColToOffset(string s, int line, int col)
    {
        int cur = 0, off = 0;
        while (off < s.Length && cur < line) { if (s[off] == '\n') cur++; off++; }
        return cur == line ? Math.Min(off + col, s.Length - 1) : -1;
    }

    private static object?[] MakeArgs(ParameterInfo[] pars, int firstArg)
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
internal class BMethodInfo { public string Name = ""; public List<BParamInfo> Params = new(); public string? ReturnType; public string? Doc; }
internal class BParamInfo { public string Name = ""; public string TypeName = "Variant"; public bool IsVar; }
