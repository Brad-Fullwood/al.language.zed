// AlBridge — Thin .NET reflection wrapper for CodeAnalysis.dll
//
// Loaded in-process by Rust via netcorehost. NOT a standalone program.
// Provides [UnmanagedCallersOnly] entry points for JSON-in/JSON-out communication.
//
// Architecture: Rust (all logic) -> netcorehost -> this DLL -> CodeAnalysis.dll

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

                var requestedDir = Path.GetDirectoryName(path)
                    ?? throw new InvalidOperationException("CodeAnalysis assembly has no parent directory.");
                var codeAnalysis = BuildForThisRuntime(path);
                var alExtDir = Path.GetDirectoryName(codeAnalysis)!;

                var context = new ToolchainLoadContext(alExtDir, Path.Combine(alExtDir, "..", "Analyzers"));
                var asm = context.LoadFromAssemblyPath(codeAnalysis);
                var bridge = new CodeAnalysisBridge(asm, alExtDir, context, requestedDir);
                _bridge = bridge;
                _codeAnalysisPath = path;
                _lastInitError = null;
                return 0;
            }
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
    /// The CodeAnalysis build for the newest framework this runtime can run.
    /// A dotnet tool lays its builds out as <c>tools/&lt;framework&gt;/any</c>,
    /// and the AL 18 tool has <c>net8.0</c> and <c>net10.0</c>. Discovery picks
    /// <c>net8.0</c>, which runs everywhere. On .NET 10 the <c>net10.0</c> build
    /// is used, so analyzers built for .NET 10 can load. Any other layout is
    /// used as given.
    /// </summary>
    internal static string BuildForThisRuntime(string codeAnalysisPath)
    {
        var anyDir = Path.GetDirectoryName(codeAnalysisPath);
        var frameworkDir = anyDir == null ? null : Path.GetDirectoryName(anyDir);
        var toolsDir = frameworkDir == null ? null : Path.GetDirectoryName(frameworkDir);
        if (anyDir == null || toolsDir == null || Path.GetFileName(anyDir) != "any" || Path.GetFileName(toolsDir) != "tools")
            return codeAnalysisPath;

        var fileName = Path.GetFileName(codeAnalysisPath);
        var runtimeMajor = Environment.Version.Major;
        var best = Directory.EnumerateDirectories(toolsDir, "net*.0")
            .Select(dir => (dir, major: int.TryParse(Path.GetFileName(dir)[3..^2], out var m) ? m : -1))
            .Where(build => build.major > 0 && build.major <= runtimeMajor
                && File.Exists(Path.Combine(build.dir, "any", fileName)))
            .OrderByDescending(build => build.major)
            .Select(build => Path.Combine(build.dir, "any", fileName))
            .FirstOrDefault();
        return best ?? codeAnalysisPath;
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
                    "analyzeProject" => _bridge?.HandleAnalyzeProject(prms),
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

/// <summary>
/// Loads the AL toolchain with the library versions it ships. AL 18's
/// CodeAnalysis references System.Collections.Immutable 9.0 and
/// System.Text.Json 10.0, which it carries in its own folder. This bridge runs
/// on the .NET 8 shared framework, whose older copies are already loaded in the
/// default context, so loading the toolchain there fails on every compiler type.
/// A library found in a probe folder loads here. Anything else, the runtime
/// itself included, comes from the default context.
/// </summary>
internal sealed class ToolchainLoadContext : System.Runtime.Loader.AssemblyLoadContext
{
    private readonly List<string> _folders;

    public ToolchainLoadContext(params string[] folders) : base("AL toolchain")
    {
        _folders = folders.Where(Directory.Exists).Select(Path.GetFullPath).ToList();
    }

    /// <summary>Probe an analyzer's folder too, for the libraries it brings.</summary>
    public void AddFolder(string folder)
    {
        var full = Path.GetFullPath(folder);
        if (Directory.Exists(full) && !_folders.Contains(full)) _folders.Add(full);
    }

    protected override Assembly? Load(AssemblyName assemblyName)
    {
        foreach (var folder in _folders)
        {
            var candidate = Path.Combine(folder, assemblyName.Name + ".dll");
            if (File.Exists(candidate)) return LoadFromAssemblyPath(candidate);
        }
        return null;
    }
}

internal class CodeAnalysisBridge
{
    private readonly Assembly _asm;
    private readonly string _alExtDir;
    private readonly ToolchainLoadContext _loadContext;
    // The toolchain folder the caller named. A built-in cop it passes from
    // there is loaded from the build in use (_alExtDir) instead.
    private readonly string _requestedDir;
    // ImmutableArray as the toolchain sees it, which may be a newer version
    // than the bridge's own.
    private readonly Type _immutableArrayType;

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

    public CodeAnalysisBridge(Assembly asm, string alExtDir, ToolchainLoadContext loadContext, string requestedDir)
    {
        _asm = asm;
        _alExtDir = alExtDir;
        _requestedDir = Path.GetFullPath(requestedDir);
        _loadContext = loadContext;
        _immutableArrayType = loadContext.LoadFromAssemblyName(new AssemblyName("System.Collections.Immutable"))
            .GetType("System.Collections.Immutable.ImmutableArray", throwOnError: true)!;

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

        var projectRoot = GetOptionalString(prms, "projectRoot");
        if (!string.IsNullOrEmpty(projectRoot))
        {
            var inProject = AnalyzeInProject(projectRoot, pkgCache, file, source, ReadOpenDocuments(prms), analyzers);
            if (inProject != null) return inProject;
        }

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

    /// <summary>
    /// Compiler and analyzer diagnostics of every file in the project at
    /// <c>projectRoot</c>, as alc reports them for a build. Diagnostics that
    /// belong to no file are left out. A folder without a usable
    /// app.json returns no diagnostics.
    /// </summary>
    public object? HandleAnalyzeProject(JsonElement prms)
    {
        var root = GetOptionalString(prms, "projectRoot");
        var pkgCache = GetOptionalString(prms, "packageCache");
        var analyzers = new List<string>();
        if (prms.TryGetProperty("analyzers", out var a) && a.ValueKind == JsonValueKind.Array)
            foreach (var item in a.EnumerateArray()) { var n = item.GetString(); if (n != null) analyzers.Add(n); }
        if (string.IsNullOrEmpty(root)) return Array.Empty<object>();
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
        if (!File.Exists(Path.Combine(root, "app.json"))) return Array.Empty<object>();

        var project = GetOrCreateProject(root, pkgCache);
        if (project == null) return Array.Empty<object>();
        UpdateProjectTrees(project, root, ReadOpenDocuments(prms));

        var diagnostics = ExtractDiagnostics(project.Compilation, "", anyTree: true);
        if (analyzers.Count > 0)
            diagnostics.AddRange(RunAnalyzers(project.Compilation, analyzers, anyTree: true));
        return diagnostics;
    }

    // ── Project compilation ─────────────────────────────────────────────
    // A file inside a project is compiled the way alc compiles it: with every
    // .al file under the project folder and the app.json dependencies loaded
    // from the package cache. Compiled alone, a file reports every dependency
    // table and every sibling object as missing (AL0185, AL0118, AL0791).
    //
    // The compilation is kept per project and updated one syntax tree at a
    // time, so the dependency symbols load on the first request only.

    private const int MaxCachedProjects = 4;
    private static readonly StringComparer PathComparer =
        OperatingSystem.IsWindows() ? StringComparer.OrdinalIgnoreCase : StringComparer.Ordinal;
    private readonly Dictionary<string, ProjectCompilation> _projects = new(PathComparer);
    private long _projectClock;

    private sealed class ProjectCompilation
    {
        public ProjectCompilation(string stamp, object? parseOptions, object compilation)
        {
            Stamp = stamp;
            ParseOptions = parseOptions;
            Compilation = compilation;
        }

        // app.json text plus the package cache listing. A change to either
        // rebuilds the compilation.
        public string Stamp { get; }
        public object? ParseOptions { get; }
        public object Compilation { get; set; }
        public Dictionary<string, ProjectTree> Trees { get; } = new(PathComparer);
        public long LastUsed { get; set; }
    }

    // A tree parsed from disk records the file's write time and length. A tree
    // parsed from an editor buffer records length -1, so the file is read
    // again once the buffer stops being sent.
    private sealed record ProjectTree(object Tree, string Text, DateTime WriteTimeUtc, long Length);

    private static Dictionary<string, string> ReadOpenDocuments(JsonElement prms)
    {
        var documents = new Dictionary<string, string>(PathComparer);
        if (!prms.TryGetProperty("openDocuments", out var list) || list.ValueKind != JsonValueKind.Array)
            return documents;
        foreach (var item in list.EnumerateArray())
        {
            var path = item.TryGetProperty("file", out var f) ? f.GetString() : null;
            var text = item.TryGetProperty("source", out var s) ? s.GetString() : null;
            if (!string.IsNullOrEmpty(path) && text != null) documents[Path.GetFullPath(path)] = text;
        }
        return documents;
    }

    private static bool IsUnder(string path, string root)
    {
        var prefix = root.EndsWith(Path.DirectorySeparatorChar) ? root : root + Path.DirectorySeparatorChar;
        return path.StartsWith(prefix, OperatingSystem.IsWindows() ? StringComparison.OrdinalIgnoreCase : StringComparison.Ordinal);
    }

    /// <summary>
    /// Analyze <paramref name="file"/> as part of the project at
    /// <paramref name="root"/>. Returns null when the folder holds no usable
    /// app.json or the file lies outside it, so the caller compiles the file
    /// alone.
    /// </summary>
    private List<object>? AnalyzeInProject(string root, string pkgCache, string file, string source,
        Dictionary<string, string> openDocuments, List<string> analyzers)
    {
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
        if (!File.Exists(Path.Combine(root, "app.json"))) return null;
        var target = Path.GetFullPath(file);
        if (!IsUnder(target, root)) return null;
        openDocuments[target] = source;

        var project = GetOrCreateProject(root, pkgCache);
        if (project == null) return null;
        UpdateProjectTrees(project, root, openDocuments);

        var tree = project.Trees[target].Tree;
        var model = GetSemanticModel(project.Compilation, tree);
        var diagnostics = ExtractDiagnostics(model, file, tree);
        if (analyzers.Count > 0) diagnostics.AddRange(RunAnalyzers(project.Compilation, analyzers, tree));
        return diagnostics;
    }

    /// <summary>
    /// The project compilation and the syntax tree of <paramref name="file"/>
    /// with <paramref name="source"/> as its text, when the request names the
    /// project the file belongs to (<c>projectRoot</c>, <c>packageCache</c>,
    /// <c>openDocuments</c>). Hover and completion then see the project's
    /// other files and its dependencies, as diagnostics do. A
    /// <paramref name="probe"/> text replaces the file for this request only:
    /// completion inserts a placeholder member that must not reach the cached
    /// compilation. Null outside a project.
    /// </summary>
    private (object Compilation, object Tree)? ProjectCompilationFor(
        JsonElement prms, string file, string source, string? probe = null)
    {
        var root = GetOptionalString(prms, "projectRoot");
        if (string.IsNullOrEmpty(root)) return null;
        root = Path.TrimEndingDirectorySeparator(Path.GetFullPath(root));
        if (!File.Exists(Path.Combine(root, "app.json"))) return null;
        var target = Path.GetFullPath(file);
        if (!IsUnder(target, root)) return null;
        var project = GetOrCreateProject(root, GetOptionalString(prms, "packageCache"));
        if (project == null) return null;

        var openDocuments = ReadOpenDocuments(prms);
        openDocuments[target] = source;
        UpdateProjectTrees(project, root, openDocuments);
        var tree = project.Trees[target].Tree;
        if (probe == null || probe == source) return (project.Compilation, tree);

        var probeTree = ParseProjectTree(probe, target, project);
        var replace = project.Compilation.GetType().GetMethod("ReplaceSyntaxTree", BindingFlags.Public | BindingFlags.Instance)
            ?? throw new MissingMethodException("Compilation.ReplaceSyntaxTree was not found.");
        var compilation = replace.Invoke(project.Compilation, new[] { tree, probeTree })
            ?? throw new InvalidOperationException("Compilation.ReplaceSyntaxTree returned null.");
        return (compilation, probeTree);
    }

    private object GetSemanticModel(object comp, object tree)
    {
        var getSM = comp.GetType().GetMethods(BindingFlags.Instance | BindingFlags.Public)
            .Where(m => m.Name == "GetSemanticModel")
            .Where(m => m.GetParameters().Length >= 1 && m.GetParameters()[0].ParameterType.IsAssignableFrom(tree.GetType()))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault()
            ?? throw new MissingMemberException("CodeAnalysis does not expose a semantic model.");
        return getSM.Invoke(comp, MakeArgs(getSM.GetParameters(), tree))
            ?? throw new InvalidOperationException("CodeAnalysis returned no semantic model.");
    }

    private static string ProjectStamp(string root, string pkgCache)
    {
        var stamp = new StringBuilder(File.ReadAllText(Path.Combine(root, "app.json")));
        if (!string.IsNullOrEmpty(pkgCache) && Directory.Exists(pkgCache))
        {
            foreach (var app in Directory.EnumerateFiles(pkgCache, "*.app").OrderBy(p => p, StringComparer.Ordinal))
            {
                var info = new FileInfo(app);
                stamp.Append('\n').Append(info.Name).Append('|').Append(info.Length)
                    .Append('|').Append(info.LastWriteTimeUtc.Ticks);
            }
        }
        return stamp.ToString();
    }

    private ProjectCompilation? GetOrCreateProject(string root, string pkgCache)
    {
        var stamp = ProjectStamp(root, pkgCache);
        if (_projects.TryGetValue(root, out var cached) && cached.Stamp == stamp)
        {
            cached.LastUsed = ++_projectClock;
            return cached;
        }
        _projects.Remove(root);

        // alc's own command-line parser reads app.json into the compilation
        // options, the parse options (runtime, preprocessor symbols) and the
        // manifest, so this compilation is configured exactly as alc's is.
        var parserType = F("Microsoft.Dynamics.Nav.CodeAnalysis.CommandLine.CommandLineParser")
            ?? throw new MissingMemberException("CodeAnalysis does not expose CommandLineParser.");
        var parser = parserType.GetField("Default", BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static)?.GetValue(null)
            ?? throw new MissingMemberException("CommandLineParser.Default was not found.");
        var parse = parserType.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Instance)
            .Where(m => m.Name == "Parse" && m.GetParameters().Length >= 2
                && m.GetParameters()[0].ParameterType == typeof(IEnumerable<string>)
                && m.GetParameters()[1].ParameterType == typeof(string))
            .OrderBy(m => m.GetParameters().Length)
            .FirstOrDefault()
            ?? throw new MissingMethodException("CommandLineParser.Parse(IEnumerable<string>, string) was not found.");
        var cliArgs = new List<string> { $"/project:{root}" };
        if (!string.IsNullOrEmpty(pkgCache)) cliArgs.Add($"/packagecachepath:{pkgCache}");
        var parseArgs = new object?[parse.GetParameters().Length];
        parseArgs[0] = cliArgs;
        parseArgs[1] = root;
        var arguments = parse.Invoke(parser, parseArgs)
            ?? throw new InvalidOperationException("CommandLineParser returned no arguments.");

        var manifest = Prop(arguments, "ProjectManifest");
        var appManifest = Prop(manifest, "AppManifest");
        if (manifest == null || appManifest == null) return null;

        var create = _compilationType?.GetMethods(BindingFlags.Public | BindingFlags.Static)
            .Where(m => m.Name == "Create")
            .Where(m => m.GetParameters().Any(p => p.Name == "syntaxTrees") && m.GetParameters().Any(p => p.Name == "options"))
            .OrderByDescending(m => m.GetParameters().Length)
            .FirstOrDefault()
            ?? throw new MissingMethodException("Compilation.Create(moduleName, …, syntaxTrees, options) was not found.");
        var values = new Dictionary<string, object?>
        {
            ["moduleName"] = Prop(appManifest, "AppName") ?? "",
            ["publisher"] = Prop(appManifest, "AppPublisher"),
            ["version"] = Prop(appManifest, "AppVersion"),
            ["appId"] = Prop(appManifest, "AppId"),
            ["alternateIds"] = Prop(appManifest, "AppAlternateIds"),
            ["options"] = Prop(arguments, "CompilationOptions"),
            ["syntaxTrees"] = Array.CreateInstance(_syntaxTreeType!, 0),
        };
        var createArgs = create.GetParameters().Select(p =>
            p.Name != null && values.TryGetValue(p.Name, out var v) && v != null && p.ParameterType.IsInstanceOfType(v)
                ? v
                : (p.HasDefaultValue ? p.DefaultValue : null)).ToArray();
        var comp = create.Invoke(null, createArgs)
            ?? throw new InvalidOperationException("Compilation.Create returned no compilation.");

        // Analyzers read app.json through the compilation's file system
        // (Microsoft's ManifestHelper), as alc and the AL extension provide
        // it. Without one, rules on the manifest report nothing: LinterCop's
        // runtime check LC0033, PerTenantExtensionCop's object ID range.
        try
        {
            var fileSystemType = F("Microsoft.Dynamics.Nav.CodeAnalysis.RelativeFileSystem");
            if (fileSystemType != null && Activator.CreateInstance(fileSystemType, root) is { } fileSystem)
                comp = InvokeSingle(comp, "WithFileSystem", fileSystem);
        }
        catch (Exception e)
        {
            Console.Error.WriteLine($"project file system not attached: {e.Message}");
        }

        // Same rule as alc: references need a package cache to load from.
        var caches = (Prop(arguments, "PackageCacheDirectories") as IEnumerable<string>)?.ToArray() ?? Array.Empty<string>();
        var getRefs = manifest.GetType().GetMethod("GetAllReferences", BindingFlags.Public | BindingFlags.Instance);
        var references = getRefs?.Invoke(manifest, new object?[getRefs.GetParameters().Length]);
        if (references != null && caches.Length > 0)
        {
            comp = InvokeSingle(comp, "AddReferences", references);
            var factory = F("Microsoft.Dynamics.Nav.CodeAnalysis.SymbolReference.ReferenceLoaderFactory")
                ?? throw new MissingMemberException("CodeAnalysis does not expose ReferenceLoaderFactory.");
            var createLoader = factory.GetMethod("CreateReferenceLoader", BindingFlags.Public | BindingFlags.Static, null,
                    new[] { typeof(IEnumerable<string>) }, null)
                ?? throw new MissingMethodException("ReferenceLoaderFactory.CreateReferenceLoader(IEnumerable<string>) was not found.");
            var loader = createLoader.Invoke(null, new object[] { caches })
                ?? throw new InvalidOperationException("ReferenceLoaderFactory returned no loader.");
            comp = InvokeSingle(comp, "WithReferenceLoader", loader);
        }

        if (_projects.Count >= MaxCachedProjects)
            _projects.Remove(_projects.MinBy(kv => kv.Value.LastUsed).Key);
        var project = new ProjectCompilation(stamp, Prop(arguments, "ParseOptions"), comp) { LastUsed = ++_projectClock };
        _projects[root] = project;
        return project;
    }

    /// <summary>Call the one-argument instance method whose parameter accepts <paramref name="arg"/>.</summary>
    private static object InvokeSingle(object target, string name, object arg)
    {
        var method = target.GetType().GetMethods(BindingFlags.Public | BindingFlags.Instance)
            .FirstOrDefault(m => m.Name == name && m.GetParameters().Length == 1
                && m.GetParameters()[0].ParameterType.IsInstanceOfType(arg))
            ?? throw new MissingMethodException($"{target.GetType().Name}.{name}({arg.GetType().Name}) was not found.");
        return method.Invoke(target, new[] { arg })
            ?? throw new InvalidOperationException($"{target.GetType().Name}.{name} returned null.");
    }

    private Array TreeArray(IReadOnlyList<object> trees)
    {
        var array = Array.CreateInstance(_syntaxTreeType!, trees.Count);
        for (int i = 0; i < trees.Count; i++) array.SetValue(trees[i], i);
        return array;
    }

    /// <summary>
    /// Bring the project's syntax trees in line with the .al files under
    /// <paramref name="root"/> (as alc enumerates them) and the editor
    /// buffers in <paramref name="openDocuments"/>.
    /// </summary>
    private void UpdateProjectTrees(ProjectCompilation project, string root, Dictionary<string, string> openDocuments)
    {
        var paths = new HashSet<string>(PathComparer);
        foreach (var path in Directory.EnumerateFiles(root, "*.al", SearchOption.AllDirectories))
            paths.Add(Path.GetFullPath(path));
        foreach (var path in openDocuments.Keys)
            if (IsUnder(path, root) && path.EndsWith(".al", StringComparison.OrdinalIgnoreCase)) paths.Add(path);

        var removed = new List<object>();
        var added = new List<object>();
        var replaced = new List<(object Old, object New)>();

        foreach (var path in project.Trees.Keys.Where(p => !paths.Contains(p)).ToList())
        {
            removed.Add(project.Trees[path].Tree);
            project.Trees.Remove(path);
        }

        foreach (var path in paths)
        {
            project.Trees.TryGetValue(path, out var existing);
            ProjectTree next;
            if (openDocuments.TryGetValue(path, out var buffer))
            {
                if (existing != null && existing.Text == buffer)
                {
                    project.Trees[path] = existing with { WriteTimeUtc = default, Length = -1 };
                    continue;
                }
                next = new ProjectTree(ParseProjectTree(buffer, path, project), buffer, default, -1);
            }
            else
            {
                FileInfo info;
                string text;
                try
                {
                    info = new FileInfo(path);
                    if (existing != null && existing.Length >= 0
                        && existing.Length == info.Length && existing.WriteTimeUtc == info.LastWriteTimeUtc)
                        continue;
                    text = File.ReadAllText(path);
                }
                catch (IOException)
                {
                    if (existing != null) { removed.Add(existing.Tree); project.Trees.Remove(path); }
                    continue;
                }
                if (existing != null && existing.Text == text)
                {
                    project.Trees[path] = existing with { WriteTimeUtc = info.LastWriteTimeUtc, Length = info.Length };
                    continue;
                }
                next = new ProjectTree(ParseProjectTree(text, path, project), text, info.LastWriteTimeUtc, info.Length);
            }

            if (existing != null) replaced.Add((existing.Tree, next.Tree));
            else added.Add(next.Tree);
            project.Trees[path] = next;
        }

        var comp = project.Compilation;
        if (removed.Count > 0) comp = InvokeSingle(comp, "RemoveSyntaxTrees", TreeArray(removed));
        foreach (var (oldTree, newTree) in replaced)
        {
            var replace = comp.GetType().GetMethod("ReplaceSyntaxTree", BindingFlags.Public | BindingFlags.Instance)
                ?? throw new MissingMethodException("Compilation.ReplaceSyntaxTree was not found.");
            comp = replace.Invoke(comp, new[] { oldTree, newTree })
                ?? throw new InvalidOperationException("Compilation.ReplaceSyntaxTree returned null.");
        }
        if (added.Count > 0) comp = InvokeSingle(comp, "AddSyntaxTrees", TreeArray(added));
        project.Compilation = comp;
    }

    private object ParseProjectTree(string text, string path, ProjectCompilation project) =>
        ParseSource(text, path, project.ParseOptions)
            ?? throw new InvalidOperationException($"CodeAnalysis returned no syntax tree for '{path}'.");

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

        // Prefer caller-supplied unsaved text over disk so hover
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
        var inProject = ProjectCompilationFor(prms, file, source);
        var tree = inProject?.Tree ?? ParseSource(source, file)
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

        var comp = inProject?.Compilation ?? CreateCompilation(new[] { tree }, pkgCache)
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

        // Prefer caller-supplied unsaved text over disk; see
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

        var inProject = ProjectCompilationFor(prms, file, source, analysisSource);
        var tree = inProject?.Tree ?? ParseSource(analysisSource, file)
            ?? throw new InvalidOperationException("CodeAnalysis returned no syntax tree.");

        var comp = inProject?.Compilation ?? CreateCompilation(new[] { tree }, pkgCache)
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

    private object? ParseSource(string source, string filePath, object? parseOptions = null)
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
            else if (parseOptions != null && pt.IsInstanceOfType(parseOptions)) parseArgs[i] = parseOptions;
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

    /// <param name="onlyTree">When set, diagnostics located in any other
    /// syntax tree, or in none, are dropped.</param>
    /// <param name="anyTree">When set, diagnostics located in no syntax tree
    /// are dropped.</param>
    private List<object> ExtractDiagnostics(object src, string defaultFile, object? onlyTree = null, bool anyTree = false)
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
            if (d == null || !IsInTree(d, onlyTree) || (anyTree && !HasSourceFile(d))) continue;
            try { results.Add(ConvertDiag(d, defaultFile)); }
            catch { /* skip */ }
        }
        return results;
    }

    private static bool HasTree(object diagnostic) =>
        Prop(Prop(diagnostic, "Location"), "SourceTree") != null;

    /// <summary>Whether a finding is located in a file: a syntax tree, or a
    /// file the compiler read without parsing it as AL, such as app.json, where
    /// LinterCop reports LC0033.</summary>
    private static bool HasSourceFile(object diagnostic)
    {
        if (HasTree(diagnostic)) return true;
        var location = Prop(diagnostic, "Location");
        if (location == null) return false;
        try
        {
            var span = location.GetType()
                .GetMethod("GetLineSpan", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
                ?.Invoke(location, null);
            var path = span?.GetType().GetProperty("Path")?.GetValue(span)?.ToString();
            return !string.IsNullOrEmpty(path);
        }
        catch { return false; }
    }

    private static bool IsInTree(object diagnostic, object? tree) =>
        tree == null || ReferenceEquals(Prop(Prop(diagnostic, "Location"), "SourceTree"), tree);

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
                    // LinePosition is 0-based. Entries are 1-based, so 0 can
                    // mean a finding with no location.
                    if (start != null) { var pt = start.GetType(); line = (uint)Prop<int>(start, pt, "Line") + 1; col = (uint)Prop<int>(start, pt, "Character") + 1; }
                    var end = st.GetProperty("EndLinePosition")?.GetValue(span);
                    if (end != null) { var pt = end.GetType(); eLine = (uint)Prop<int>(end, pt, "Line") + 1; eCol = (uint)Prop<int>(end, pt, "Character") + 1; }
                }
            }
            catch { /* skip location */ }
        }

        return new { file, line, column = col, endLine = eLine, endColumn = eCol, severity = NormSev(sev), code = id, message = msg };
    }

    /// <param name="tree">When set, only this syntax tree is analyzed and
    /// only its diagnostics are returned.</param>
    /// <param name="anyTree">When set, findings located in no syntax tree
    /// are dropped.</param>
    private List<object> RunAnalyzers(object comp, List<string> names, object? tree = null, bool anyTree = false)
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
                var sameBuild = Path.Combine(_alExtDir, Path.GetFileName(dllPath));
                if (string.Equals(Path.GetDirectoryName(Path.GetFullPath(dllPath)), _requestedDir, StringComparison.Ordinal)
                    && File.Exists(sameBuild))
                    dllPath = sameBuild;
                _loadContext.AddFolder(Path.GetDirectoryName(dllPath)!);
                var aAsm = _loadContext.LoadFromAssemblyPath(Path.GetFullPath(dllPath));
                // An analyzer assembly built against another CodeAnalysis
                // version can fail to load some of its types. The ones that
                // load still run, and the rest are reported.
                Type[] types;
                try { types = aAsm.GetTypes(); }
                catch (ReflectionTypeLoadException ex)
                {
                    types = ex.Types.Where(t => t != null).ToArray()!;
                    var reasons = ex.LoaderExceptions.Where(e => e != null).Select(e => e!.Message).Distinct().Take(3);
                    results.Add(AnalyzerProblem(name, $"some analyzers could not be loaded: {string.Join(" | ", reasons)}"));
                }
                var analyzers = types
                    .Where(t => !t.IsAbstract && _diagnosticAnalyzerType.IsAssignableFrom(t))
                    .Select(t => Activator.CreateInstance(t)
                        ?? throw new InvalidOperationException($"Could not construct analyzer '{t.FullName}'."))
                    .ToArray();
                if (analyzers.Length == 0)
                    throw new InvalidOperationException($"'{dllPath}' contains no AL diagnostic analyzers that load.");
                // One run per assembly: the analysis binds the code once for
                // all of its analyzers.
                results.AddRange(RunAnalyzerBatch(comp, analyzers, tree, anyTree));
            }
            catch (Exception ex)
            {
                // One analyzer that fails is reported on its own, so the
                // compiler's diagnostics and the other analyzers still arrive.
                var root = ex;
                while (root.InnerException != null) root = root.InnerException;
                results.Add(AnalyzerProblem(name, $"{root.GetType().Name}: {root.Message}"));
            }
        }
        return results;
    }

    /// <summary>A warning at the top of the file that names an analyzer that did not run fully.</summary>
    private static object AnalyzerProblem(string analyzer, string problem) => new
    {
        file = "",
        line = 0u,
        column = 0u,
        endLine = 0u,
        endColumn = 0u,
        severity = "warning",
        code = "AL-ANALYZER",
        message = $"Analyzer '{analyzer}' {problem}",
    };

    private List<object> RunAnalyzerBatch(object comp, object[] analyzers, object? tree, bool anyTree = false)
    {
        var results = new List<object>();
        if (_diagnosticAnalyzerType == null)
            throw new MissingMemberException("CodeAnalysis does not expose DiagnosticAnalyzer.");

        var immCreate = _immutableArrayType.GetMethods(BindingFlags.Static | BindingFlags.Public)
            .FirstOrDefault(m => m.Name == "Create" && m.GetParameters().Length == 1 && m.GetParameters()[0].ParameterType.IsArray)
            ?? throw new MissingMemberException("ImmutableArray.Create<T>(T[]) was not found.");
        var gc = immCreate.MakeGenericMethod(_diagnosticAnalyzerType);
        var arr = Array.CreateInstance(_diagnosticAnalyzerType, analyzers.Length);
        for (int i = 0; i < analyzers.Length; i++) arr.SetValue(analyzers[i], i);
        var immAnalyzers = gc.Invoke(null, new object[] { arr })
            ?? throw new InvalidOperationException("ImmutableArray.Create returned null.");

        if (tree != null)
        {
            foreach (var d in DocumentAnalyzerDiagnostics(comp, tree, immAnalyzers))
                if (d != null && IsInTree(d, tree)) results.Add(ConvertDiag(d, ""));
            return results;
        }

        var cwaType = F("Microsoft.Dynamics.Nav.CodeAnalysis.Diagnostics.CompilationWithAnalyzers");
        if (cwaType == null)
            throw new MissingMemberException("CodeAnalysis does not expose CompilationWithAnalyzers.");
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
                var cwa = ctor.Invoke(args)
                    ?? throw new InvalidOperationException("CompilationWithAnalyzers could not be constructed.");
                foreach (var d in WholeCompilationDiagnostics(cwaType, cwa))
                    if (d != null && (!anyTree || HasSourceFile(d))) results.Add(ConvertDiag(d, ""));
                return results;
            }
            catch (Exception ex) { lastError = ex; }
        }
        throw new InvalidOperationException(
            "No compatible CompilationWithAnalyzers constructor succeeded.", lastError);
    }

    // Analyzer diagnostics of one syntax tree, computed the way Microsoft's AL
    // language server does for its "File" analysis scope. The public per-tree
    // methods of CompilationWithAnalyzers return nothing on a fresh instance
    // (they analyze only compilation events already queued), so the internal
    // helper the language server calls is used instead.
    private IEnumerable<object?> DocumentAnalyzerDiagnostics(object comp, object tree, object analyzers)
    {
        var helper = F("Microsoft.Dynamics.Nav.CodeAnalysis.Analyzers.AnalyzersHelper")
            ?? throw new MissingMemberException("CodeAnalysis does not expose AnalyzersHelper.");
        var method = helper.GetMethod("GetAnalyzerDiagnosticsForDocument", BindingFlags.Static | BindingFlags.Public | BindingFlags.NonPublic)
            ?? throw new MissingMethodException("AnalyzersHelper.GetAnalyzerDiagnosticsForDocument was not found.");
        var pars = method.GetParameters();
        var args = new object?[pars.Length];
        for (int i = 0; i < pars.Length; i++)
        {
            var pt = pars[i].ParameterType;
            if (pt.IsInstanceOfType(tree)) args[i] = tree;
            else if (pt.IsInstanceOfType(comp)) args[i] = comp;
            else if (pt.IsInstanceOfType(analyzers)) args[i] = analyzers;
            else if (pt == typeof(string)) args[i] = "AlBridge";
            else if (pt == typeof(CancellationToken)) args[i] = CancellationToken.None;
            else if (pars[i].HasDefaultValue) args[i] = pars[i].DefaultValue;
            else args[i] = null;
        }
        var task = method.Invoke(null, args)
            ?? throw new InvalidOperationException("AnalyzersHelper returned no task.");
        task.GetType().GetMethod("Wait", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?.Invoke(task, null);
        // The result is (CompilationDiagnostics, AnalysisDiagnostics). The
        // compiler's own diagnostics already come from the semantic model.
        var result = task.GetType().GetProperty("Result")?.GetValue(task)
            ?? throw new InvalidOperationException("AnalyzersHelper returned no result.");
        if (result.GetType().GetField("Item2")?.GetValue(result) is not System.Collections.IEnumerable analysis)
            throw new InvalidOperationException("AnalyzersHelper returned an unexpected result.");
        return analysis.Cast<object?>();
    }

    private static IEnumerable<object?> WholeCompilationDiagnostics(Type cwaType, object cwa)
    {
        var getDiags = cwaType.GetMethod("GetAnalyzerDiagnosticsAsync",
            BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?? cwaType.GetMethod("GetAllDiagnosticsAsync",
                BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?? throw new MissingMethodException("No analyzer diagnostics method was found.");
        return AwaitDiagnostics(getDiags.Invoke(cwa, null));
    }

    private static IEnumerable<object?> AwaitDiagnostics(object? task)
    {
        if (task == null) throw new InvalidOperationException("Analyzer diagnostics returned no task.");
        task.GetType().GetMethod("Wait", BindingFlags.Instance | BindingFlags.Public, null, Type.EmptyTypes, null)
            ?.Invoke(task, null);
        if (task.GetType().GetProperty("Result")?.GetValue(task) is not System.Collections.IEnumerable result)
            throw new InvalidOperationException("Analyzer diagnostics returned an unexpected result.");
        return result.Cast<object?>();
    }

    // An analyzer is an absolute path, or a built-in cop's name looked up in the
    // toolchain directory. A relative name is not tried as a path: File.Exists
    // resolves it against the working directory, which is the project folder.
    private string? ResolveAnalyzerPath(string name)
    {
        if (Path.IsPathFullyQualified(name)) return File.Exists(name) ? name : null;
        if (name.StartsWith("${", StringComparison.Ordinal) && name.EndsWith('}'))
            name = name[2..^1];
        var normalized = name.EndsWith(".dll", StringComparison.OrdinalIgnoreCase)
            ? name[..^4]
            : name;
        var productName = normalized.ToLowerInvariant() switch
        {
            "codecop" => "CodeCop",
            "appsourcecop" => "AppSourceCop",
            "uicop" => "UICop",
            "pertenantcop" or "pertenantextensioncop" => "PerTenantExtensionCop",
            _ => null,
        };
        if (productName is null) return null;
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
