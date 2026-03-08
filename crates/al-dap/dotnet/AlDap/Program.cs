// AlDap — .NET bridge for AL Debug Adapter Protocol
//
// Runs as a subprocess of the al-dap Rust crate. Handles actual communication
// with Business Central servers using Microsoft's deployment/debug DLLs.
//
// Communication: line-delimited JSON-RPC on stdin/stdout.
// The Rust side sends one JSON object per line, we respond with one JSON object per line.
//
// Loaded dynamically:
//   - Microsoft.Dynamics.Nav.Deployment.dll (from ALTool)
//   - Microsoft.Dynamics.Nav.Debug.dll (from ALTool)

using System.Reflection;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace AlDap;

class Program
{
    // Loaded assemblies (populated during connect)
    private static Assembly? _deploymentAssembly;
    private static Assembly? _debugAssembly;
    private static string _altoolDir = "";

    // Active debug session — the actual BC debug session object (reflected)
    private static object? _debugSession;

    // Cached reflected types and methods from the debug assembly.
    // Populated once during connect, reused by all handlers.
    private static Type? _sessionType;
    private static Type? _breakpointType;
    private static Type? _stackFrameType;
    private static Type? _variableType;

    // Session methods — cached MethodInfo so we reflect only once
    private static MethodInfo? _continueMethod;
    private static MethodInfo? _stepIntoMethod;
    private static MethodInfo? _stepOutMethod;
    private static MethodInfo? _stepOverMethod;
    private static MethodInfo? _getThreadsMethod;
    private static MethodInfo? _getStackTraceMethod;
    private static MethodInfo? _getScopesMethod;
    private static MethodInfo? _getVariablesMethod;
    private static MethodInfo? _setBreakpointsMethod;
    private static MethodInfo? _addBreakpointMethod;
    private static MethodInfo? _removeBreakpointMethod;
    private static MethodInfo? _clearBreakpointsMethod;
    private static MethodInfo? _evaluateMethod;
    private static MethodInfo? _disconnectMethod;
    private static MethodInfo? _attachMethod;
    private static MethodInfo? _waitForBreakpointMethod;

    // Track our own breakpoint IDs
    private static long _nextBreakpointId = 1;

    // Map from our breakpoint IDs to reflected breakpoint objects
    private static readonly Dictionary<long, object> _breakpointMap = new();

    // Map from variablesReference IDs to (frameId, scope) for variable retrieval
    private static readonly Dictionary<long, (long frameId, string scope)> _variableRefMap = new();
    private static long _nextVariableRef = 1;

    // Map from variablesReference IDs to child variable containers (for nested objects)
    private static readonly Dictionary<long, object> _containerMap = new();

    // JSON serializer options
    private static readonly JsonSerializerOptions JsonOpts = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        WriteIndented = false,
    };

    static async Task Main(string[] args)
    {
        if (args.Length < 1)
        {
            Console.Error.WriteLine("Usage: AlDap <path-to-altool-dir>");
            Environment.Exit(1);
        }

        _altoolDir = args[0];

        if (!Directory.Exists(_altoolDir))
        {
            Console.Error.WriteLine($"ALTool directory not found: {_altoolDir}");
            Environment.Exit(1);
        }

        Console.Error.WriteLine($"AlDap bridge started with ALTool at: {_altoolDir}");

        // Set up assembly resolution so we can load BC DLLs and their transitive deps
        AppDomain.CurrentDomain.AssemblyResolve += (_, resolveArgs) =>
        {
            var name = new AssemblyName(resolveArgs.Name).Name;
            if (name == null) return null;

            // Search in the ALTool directory and common subdirectories
            string[] searchPaths =
            [
                _altoolDir,
                Path.Combine(_altoolDir, "bin"),
                Path.Combine(_altoolDir, "Debug"),
            ];

            foreach (var dir in searchPaths)
            {
                var path = Path.Combine(dir, $"{name}.dll");
                if (File.Exists(path))
                {
                    try
                    {
                        return Assembly.LoadFrom(path);
                    }
                    catch (Exception ex)
                    {
                        Console.Error.WriteLine($"AssemblyResolve: failed to load {path}: {ex.Message}");
                    }
                }
            }

            return null;
        };

        // JSON-RPC loop
        using var reader = new StreamReader(Console.OpenStandardInput());
        using var writer = new StreamWriter(Console.OpenStandardOutput()) { AutoFlush = true };

        while (true)
        {
            var line = await reader.ReadLineAsync();
            if (line == null) break;
            if (string.IsNullOrWhiteSpace(line)) continue;

            try
            {
                var request = JsonNode.Parse(line);
                if (request == null) continue;

                var method = request["method"]?.GetValue<string>() ?? "";
                var id = request["id"]?.GetValue<ulong>() ?? 0;
                var parms = request["params"];

                JsonNode result;
                try
                {
                    result = method switch
                    {
                        "ping" => HandlePing(),
                        "connect" => HandleConnect(parms),
                        "disconnect" => HandleDisconnect(),
                        "setBreakpoints" => HandleSetBreakpoints(parms),
                        "continue" => HandleContinue(parms),
                        "stepIn" => HandleStepIn(parms),
                        "stepOut" => HandleStepOut(parms),
                        "stepOver" => HandleStepOver(parms),
                        "threads" => HandleThreads(),
                        "stackTrace" => HandleStackTrace(parms),
                        "scopes" => HandleScopes(parms),
                        "variables" => HandleVariables(parms),
                        "evaluate" => HandleEvaluate(parms),
                        _ => MakeError($"Unknown method: {method}")
                    };
                }
                catch (Exception ex)
                {
                    result = MakeError($"{method} failed: {ex.Message}");
                    Console.Error.WriteLine($"Error in {method}: {ex}");
                }

                var response = new JsonObject
                {
                    ["id"] = id,
                    ["result"] = result,
                };

                writer.WriteLine(response.ToJsonString(JsonOpts));
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Error parsing request: {ex.Message}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Handlers
    // -----------------------------------------------------------------------

    static JsonNode HandlePing()
    {
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleConnect(JsonNode? parms)
    {
        if (parms == null)
            return MakeError("Missing connection parameters");

        var server = parms["server"]?.GetValue<string>() ?? "";
        var serverInstance = parms["serverInstance"]?.GetValue<string>() ?? "";
        var tenant = parms["tenant"]?.GetValue<string>() ?? "default";
        var authentication = parms["authentication"]?.GetValue<string>() ?? "UserPassword";
        var breakOnError = parms["breakpointOnError"]?.GetValue<string>() ?? "All";

        Console.Error.WriteLine($"Connecting to {server}/{serverInstance} tenant={tenant} auth={authentication}");

        // Step 1: Load the debug assembly
        if (!LoadDebugAssembly())
        {
            return MakeError("Could not load Microsoft.Dynamics.Nav.Debug.dll — see stderr for details");
        }

        // Step 2: Load the deployment assembly (optional, used for server connectivity checks)
        LoadDeploymentAssembly();

        // Step 3: Discover the debug session type via reflection
        if (!DiscoverDebugTypes())
        {
            return MakeError("Could not find a usable debug session type in the debug assembly — see stderr for API dump");
        }

        // Step 4: Create and connect the debug session
        try
        {
            _debugSession = CreateDebugSession(server, serverInstance, tenant, authentication, breakOnError);
            if (_debugSession == null)
            {
                return MakeError("Failed to create debug session — see stderr for details");
            }

            Console.Error.WriteLine($"Debug session created: {_debugSession.GetType().FullName}");

            // Step 5: Attach the debugger to the BC session
            AttachDebugger();

            return new JsonObject { ["status"] = "connected" };
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            Console.Error.WriteLine($"Connection failed (TargetInvocationException): {inner}");
            return MakeError($"Connection failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Connection failed: {ex}");
            return MakeError($"Connection failed: {ex.Message}");
        }
    }

    static JsonNode HandleDisconnect()
    {
        if (_debugSession != null)
        {
            try
            {
                // Try the discovered disconnect method
                if (_disconnectMethod != null)
                {
                    _disconnectMethod.Invoke(_debugSession, null);
                    Console.Error.WriteLine("Debug session disconnected via Disconnect()");
                }
                else
                {
                    // Try common method names
                    TryInvoke(_debugSession, "Disconnect") ||
                    TryInvoke(_debugSession, "Close") ||
                    TryInvoke(_debugSession, "Dispose") ||
                    TryInvoke(_debugSession, "Stop");
                }

                // If the session is IDisposable, dispose it
                if (_debugSession is IDisposable disposable)
                {
                    disposable.Dispose();
                    Console.Error.WriteLine("Debug session disposed");
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning during disconnect: {ex.Message}");
            }
        }

        // Clear all state
        _debugSession = null;
        _sessionType = null;
        _breakpointMap.Clear();
        _variableRefMap.Clear();
        _containerMap.Clear();
        _nextBreakpointId = 1;
        _nextVariableRef = 1;

        Console.Error.WriteLine("Disconnected and state cleared");
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleSetBreakpoints(JsonNode? parms)
    {
        var file = parms?["file"]?.GetValue<string>() ?? "";
        var breakpoints = parms?["breakpoints"]?.AsArray() ?? new JsonArray();

        Console.Error.WriteLine($"Setting {breakpoints.Count} breakpoints in {file}");

        var responseBreakpoints = new JsonArray();

        if (_debugSession == null)
        {
            // No active session — return unverified breakpoints
            foreach (var bp in breakpoints)
            {
                var line = bp?["line"]?.GetValue<int>() ?? 0;
                responseBreakpoints.Add(new JsonObject
                {
                    ["id"] = _nextBreakpointId++,
                    ["verified"] = false,
                    ["line"] = line,
                    ["message"] = "Not connected to BC server",
                    ["source"] = new JsonObject { ["path"] = file },
                });
            }
            return new JsonObject { ["breakpoints"] = responseBreakpoints };
        }

        // Clear existing breakpoints for this file first
        try
        {
            ClearBreakpointsForFile(file);
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Warning: failed to clear existing breakpoints for {file}: {ex.Message}");
        }

        foreach (var bp in breakpoints)
        {
            var line = bp?["line"]?.GetValue<int>() ?? 0;
            var condition = bp?["condition"]?.GetValue<string>();
            var hitCondition = bp?["hitCondition"]?.GetValue<string>();
            var logMessage = bp?["logMessage"]?.GetValue<string>();

            try
            {
                var bpId = _nextBreakpointId++;
                var (verified, actualLine, message) = SetBreakpointViaReflection(file, line, condition, hitCondition, logMessage, bpId);

                responseBreakpoints.Add(new JsonObject
                {
                    ["id"] = bpId,
                    ["verified"] = verified,
                    ["line"] = actualLine,
                    ["message"] = message,
                    ["source"] = new JsonObject { ["path"] = file },
                });
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Failed to set breakpoint at {file}:{line}: {ex.Message}");
                responseBreakpoints.Add(new JsonObject
                {
                    ["id"] = _nextBreakpointId++,
                    ["verified"] = false,
                    ["line"] = line,
                    ["message"] = $"Failed: {ex.Message}",
                    ["source"] = new JsonObject { ["path"] = file },
                });
            }
        }

        return new JsonObject { ["breakpoints"] = responseBreakpoints };
    }

    static JsonNode HandleContinue(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Continue thread {threadId}");

        if (_debugSession == null)
            return MakeError("Not connected — no active debug session");

        try
        {
            if (_continueMethod != null)
            {
                _continueMethod.Invoke(_debugSession, null);
            }
            else
            {
                // Try common method names for continue
                if (!TryInvoke(_debugSession, "Continue") &&
                    !TryInvoke(_debugSession, "Go") &&
                    !TryInvoke(_debugSession, "Resume"))
                {
                    return MakeError("No Continue/Go/Resume method found on debug session");
                }
            }

            return new JsonObject { ["status"] = "ok" };
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            Console.Error.WriteLine($"Continue failed: {inner}");
            return MakeError($"Continue failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Continue failed: {ex}");
            return MakeError($"Continue failed: {ex.Message}");
        }
    }

    static JsonNode HandleStepIn(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step in thread {threadId}");

        if (_debugSession == null)
            return MakeError("Not connected — no active debug session");

        try
        {
            if (_stepIntoMethod != null)
            {
                _stepIntoMethod.Invoke(_debugSession, null);
            }
            else
            {
                if (!TryInvoke(_debugSession, "StepInto") &&
                    !TryInvoke(_debugSession, "StepIn"))
                {
                    return MakeError("No StepInto/StepIn method found on debug session");
                }
            }

            return new JsonObject { ["status"] = "ok" };
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            return MakeError($"StepIn failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            return MakeError($"StepIn failed: {ex.Message}");
        }
    }

    static JsonNode HandleStepOut(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step out thread {threadId}");

        if (_debugSession == null)
            return MakeError("Not connected — no active debug session");

        try
        {
            if (_stepOutMethod != null)
            {
                _stepOutMethod.Invoke(_debugSession, null);
            }
            else
            {
                if (!TryInvoke(_debugSession, "StepOut"))
                {
                    return MakeError("No StepOut method found on debug session");
                }
            }

            return new JsonObject { ["status"] = "ok" };
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            return MakeError($"StepOut failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            return MakeError($"StepOut failed: {ex.Message}");
        }
    }

    static JsonNode HandleStepOver(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step over thread {threadId}");

        if (_debugSession == null)
            return MakeError("Not connected — no active debug session");

        try
        {
            if (_stepOverMethod != null)
            {
                _stepOverMethod.Invoke(_debugSession, null);
            }
            else
            {
                if (!TryInvoke(_debugSession, "StepOver") &&
                    !TryInvoke(_debugSession, "Next"))
                {
                    return MakeError("No StepOver/Next method found on debug session");
                }
            }

            return new JsonObject { ["status"] = "ok" };
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            return MakeError($"StepOver failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            return MakeError($"StepOver failed: {ex.Message}");
        }
    }

    static JsonNode HandleThreads()
    {
        var threads = new JsonArray();

        if (_debugSession == null)
        {
            // Return a single dummy thread when not connected
            threads.Add(new JsonObject { ["id"] = 1, ["name"] = "BC Session (not connected)" });
            return new JsonObject { ["threads"] = threads };
        }

        try
        {
            var threadList = GetThreadsViaReflection();
            if (threadList.Count == 0)
            {
                // Always return at least one thread — BC always has a main session
                threads.Add(new JsonObject { ["id"] = 1, ["name"] = "BC Session" });
            }
            else
            {
                foreach (var (id, name) in threadList)
                {
                    threads.Add(new JsonObject { ["id"] = id, ["name"] = name });
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to get threads: {ex.Message}");
            threads.Add(new JsonObject { ["id"] = 1, ["name"] = "BC Session" });
        }

        return new JsonObject { ["threads"] = threads };
    }

    static JsonNode HandleStackTrace(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Stack trace for thread {threadId}");

        var stackFrames = new JsonArray();

        if (_debugSession == null)
            return new JsonObject { ["stackFrames"] = stackFrames };

        try
        {
            var frames = GetStackTraceViaReflection(threadId);
            foreach (var frame in frames)
            {
                var frameObj = new JsonObject
                {
                    ["id"] = frame.id,
                    ["name"] = frame.name,
                    ["line"] = frame.line,
                    ["column"] = frame.column,
                };

                if (frame.sourcePath != null)
                {
                    frameObj["source"] = new JsonObject
                    {
                        ["path"] = frame.sourcePath,
                        ["name"] = Path.GetFileName(frame.sourcePath),
                    };
                }

                stackFrames.Add(frameObj);
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to get stack trace: {ex.Message}");
        }

        return new JsonObject { ["stackFrames"] = stackFrames };
    }

    static JsonNode HandleScopes(JsonNode? parms)
    {
        var frameId = parms?["frameId"]?.GetValue<long>() ?? 0;
        Console.Error.WriteLine($"Scopes for frame {frameId}");

        var scopes = new JsonArray();

        if (_debugSession == null)
        {
            // Return standard scope structure even without a session
            var localRef = _nextVariableRef++;
            var globalRef = _nextVariableRef++;
            _variableRefMap[localRef] = (frameId, "Locals");
            _variableRefMap[globalRef] = (frameId, "Globals");

            scopes.Add(new JsonObject
            {
                ["name"] = "Locals",
                ["variablesReference"] = localRef,
                ["expensive"] = false,
            });
            scopes.Add(new JsonObject
            {
                ["name"] = "Globals",
                ["variablesReference"] = globalRef,
                ["expensive"] = false,
            });
            return new JsonObject { ["scopes"] = scopes };
        }

        try
        {
            var scopeList = GetScopesViaReflection(frameId);

            if (scopeList.Count == 0)
            {
                // Default scopes if reflection doesn't find any
                var localRef = _nextVariableRef++;
                var globalRef = _nextVariableRef++;
                _variableRefMap[localRef] = (frameId, "Locals");
                _variableRefMap[globalRef] = (frameId, "Globals");

                scopes.Add(new JsonObject
                {
                    ["name"] = "Locals",
                    ["variablesReference"] = localRef,
                    ["expensive"] = false,
                });
                scopes.Add(new JsonObject
                {
                    ["name"] = "Globals",
                    ["variablesReference"] = globalRef,
                    ["expensive"] = false,
                });
            }
            else
            {
                foreach (var (name, varRef, expensive) in scopeList)
                {
                    scopes.Add(new JsonObject
                    {
                        ["name"] = name,
                        ["variablesReference"] = varRef,
                        ["expensive"] = expensive,
                    });
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to get scopes: {ex.Message}");
        }

        return new JsonObject { ["scopes"] = scopes };
    }

    static JsonNode HandleVariables(JsonNode? parms)
    {
        var reference = parms?["variablesReference"]?.GetValue<long>() ?? 0;
        Console.Error.WriteLine($"Variables for reference {reference}");

        var variables = new JsonArray();

        if (_debugSession == null)
            return new JsonObject { ["variables"] = variables };

        try
        {
            var varList = GetVariablesViaReflection(reference);

            foreach (var v in varList)
            {
                var varObj = new JsonObject
                {
                    ["name"] = v.name,
                    ["value"] = v.value,
                    ["variablesReference"] = v.childRef,
                };

                if (v.type != null)
                    varObj["type"] = v.type;

                variables.Add(varObj);
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to get variables: {ex.Message}");
        }

        return new JsonObject { ["variables"] = variables };
    }

    static JsonNode HandleEvaluate(JsonNode? parms)
    {
        var expression = parms?["expression"]?.GetValue<string>() ?? "";
        var frameId = parms?["frameId"]?.GetValue<long>();
        Console.Error.WriteLine($"Evaluate: {expression} (frame={frameId})");

        if (_debugSession == null)
            return MakeError("Not connected — cannot evaluate expressions");

        try
        {
            var (result, type) = EvaluateViaReflection(expression, frameId);
            var response = new JsonObject { ["result"] = result };
            if (type != null)
                response["type"] = type;
            // variablesReference = 0 means no children
            response["variablesReference"] = 0;
            return response;
        }
        catch (TargetInvocationException tie)
        {
            var inner = tie.InnerException ?? tie;
            Console.Error.WriteLine($"Evaluate failed: {inner}");
            return MakeError($"Evaluation failed: {inner.Message}");
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Evaluate failed: {ex}");
            return MakeError($"Evaluation failed: {ex.Message}");
        }
    }

    // -----------------------------------------------------------------------
    // Assembly Loading
    // -----------------------------------------------------------------------

    /// Load Microsoft.Dynamics.Nav.Debug.dll from the ALTool directory.
    static bool LoadDebugAssembly()
    {
        string[] candidatePaths =
        [
            Path.Combine(_altoolDir, "Microsoft.Dynamics.Nav.Debug.dll"),
            Path.Combine(_altoolDir, "bin", "Microsoft.Dynamics.Nav.Debug.dll"),
            Path.Combine(_altoolDir, "Debug", "Microsoft.Dynamics.Nav.Debug.dll"),
        ];

        foreach (var path in candidatePaths)
        {
            if (!File.Exists(path)) continue;

            try
            {
                _debugAssembly = Assembly.LoadFrom(path);
                Console.Error.WriteLine($"Loaded debug assembly from: {path}");
                Console.Error.WriteLine($"  FullName: {_debugAssembly.FullName}");
                return true;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Failed to load debug assembly from {path}: {ex.Message}");
            }
        }

        Console.Error.WriteLine("Debug assembly not found in any candidate path:");
        foreach (var p in candidatePaths)
            Console.Error.WriteLine($"  Checked: {p}");

        return false;
    }

    /// Load Microsoft.Dynamics.Nav.Deployment.dll (optional, for server connectivity).
    static void LoadDeploymentAssembly()
    {
        string[] candidatePaths =
        [
            Path.Combine(_altoolDir, "Microsoft.Dynamics.Nav.Deployment.dll"),
            Path.Combine(_altoolDir, "bin", "Microsoft.Dynamics.Nav.Deployment.dll"),
        ];

        foreach (var path in candidatePaths)
        {
            if (!File.Exists(path)) continue;

            try
            {
                _deploymentAssembly = Assembly.LoadFrom(path);
                Console.Error.WriteLine($"Loaded deployment assembly from: {path}");
                return;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: Could not load deployment assembly from {path}: {ex.Message}");
            }
        }

        Console.Error.WriteLine("Deployment assembly not found — server connectivity checks will be skipped");
    }

    // -----------------------------------------------------------------------
    // Type Discovery via Reflection
    // -----------------------------------------------------------------------

    /// Discover and cache all debug types and methods from the loaded assembly.
    /// Returns true if a usable session type was found.
    static bool DiscoverDebugTypes()
    {
        if (_debugAssembly == null) return false;

        Console.Error.WriteLine("Discovering debug types...");

        // Dump all exported types for debugging
        var allTypes = _debugAssembly.GetExportedTypes();
        Console.Error.WriteLine($"Debug assembly contains {allTypes.Length} exported types:");
        foreach (var t in allTypes)
        {
            Console.Error.WriteLine($"  {t.FullName}");
        }

        // Strategy 1: Look for well-known session type names
        string[] sessionTypeNames =
        [
            "Microsoft.Dynamics.Nav.Debug.DebugSession",
            "Microsoft.Dynamics.Nav.Debug.DebuggerSession",
            "Microsoft.Dynamics.Nav.Debug.ALDebugger",
            "Microsoft.Dynamics.Nav.Debug.DebugAdapter",
            "Microsoft.Dynamics.Nav.Debug.DebugClient",
            "Microsoft.Dynamics.Nav.Debug.DebugProxy",
            "Microsoft.Dynamics.Nav.Debug.NavDebugSession",
            "Microsoft.Dynamics.Nav.Debug.BCDebugSession",
        ];

        foreach (var name in sessionTypeNames)
        {
            _sessionType = _debugAssembly.GetType(name);
            if (_sessionType != null)
            {
                Console.Error.WriteLine($"Found session type (by name): {_sessionType.FullName}");
                break;
            }
        }

        // Strategy 2: Search for any type with "Debug" and "Session" in the name
        if (_sessionType == null)
        {
            _sessionType = allTypes.FirstOrDefault(t =>
                t.Name.Contains("Debug", StringComparison.OrdinalIgnoreCase) &&
                t.Name.Contains("Session", StringComparison.OrdinalIgnoreCase) &&
                t.IsClass && !t.IsAbstract);

            if (_sessionType != null)
                Console.Error.WriteLine($"Found session type (by pattern Debug+Session): {_sessionType.FullName}");
        }

        // Strategy 3: Search for any type implementing an IDebug* interface
        if (_sessionType == null)
        {
            _sessionType = allTypes.FirstOrDefault(t =>
                t.IsClass && !t.IsAbstract &&
                t.GetInterfaces().Any(i => i.Name.Contains("Debug", StringComparison.OrdinalIgnoreCase)));

            if (_sessionType != null)
                Console.Error.WriteLine($"Found session type (by IDebug* interface): {_sessionType.FullName}");
        }

        // Strategy 4: Search for any class with "Debugger" in the name
        if (_sessionType == null)
        {
            _sessionType = allTypes.FirstOrDefault(t =>
                t.IsClass && !t.IsAbstract &&
                t.Name.Contains("Debugger", StringComparison.OrdinalIgnoreCase));

            if (_sessionType != null)
                Console.Error.WriteLine($"Found session type (by Debugger name): {_sessionType.FullName}");
        }

        if (_sessionType == null)
        {
            Console.Error.WriteLine("ERROR: No usable debug session type found in the assembly");
            Console.Error.WriteLine("Dumping all types with public constructors:");
            foreach (var t in allTypes.Where(t => t.IsClass && !t.IsAbstract))
            {
                var ctors = t.GetConstructors(BindingFlags.Public | BindingFlags.Instance);
                if (ctors.Length > 0)
                {
                    Console.Error.WriteLine($"  {t.FullName}:");
                    foreach (var c in ctors)
                    {
                        var ps = string.Join(", ", c.GetParameters().Select(p => $"{p.ParameterType.Name} {p.Name}"));
                        Console.Error.WriteLine($"    ctor({ps})");
                    }
                }
            }
            return false;
        }

        // Discover methods on the session type
        DiscoverSessionMethods();

        // Discover related types
        DiscoverRelatedTypes();

        return true;
    }

    /// Cache method references for the discovered session type.
    static void DiscoverSessionMethods()
    {
        if (_sessionType == null) return;

        var flags = BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase;

        Console.Error.WriteLine($"Methods on {_sessionType.Name}:");
        foreach (var m in _sessionType.GetMethods(flags).Where(m => !m.IsSpecialName))
        {
            var ps = string.Join(", ", m.GetParameters().Select(p => $"{p.ParameterType.Name} {p.Name}"));
            Console.Error.WriteLine($"  {m.ReturnType.Name} {m.Name}({ps})");
        }

        // Execution control
        _continueMethod = FindMethod(_sessionType, flags, "Continue", "Go", "Resume", "Run");
        _stepIntoMethod = FindMethod(_sessionType, flags, "StepInto", "StepIn");
        _stepOutMethod = FindMethod(_sessionType, flags, "StepOut");
        _stepOverMethod = FindMethod(_sessionType, flags, "StepOver", "Next", "StepLine");

        // Inspection
        _getThreadsMethod = FindMethod(_sessionType, flags, "GetThreads", "GetSessions", "GetActiveSessions");
        _getStackTraceMethod = FindMethod(_sessionType, flags, "GetStackTrace", "GetCallStack", "GetStack", "GetStackFrames");
        _getScopesMethod = FindMethod(_sessionType, flags, "GetScopes", "GetVariableScopes");
        _getVariablesMethod = FindMethod(_sessionType, flags, "GetVariables", "GetVariableValues", "GetLocals");

        // Breakpoints
        _setBreakpointsMethod = FindMethod(_sessionType, flags, "SetBreakpoints", "SetBreakpoint");
        _addBreakpointMethod = FindMethod(_sessionType, flags, "AddBreakpoint", "SetBreakpoint", "InsertBreakpoint");
        _removeBreakpointMethod = FindMethod(_sessionType, flags, "RemoveBreakpoint", "DeleteBreakpoint", "ClearBreakpoint");
        _clearBreakpointsMethod = FindMethod(_sessionType, flags, "ClearBreakpoints", "RemoveAllBreakpoints", "ClearAllBreakpoints");

        // Evaluation
        _evaluateMethod = FindMethod(_sessionType, flags, "Evaluate", "EvaluateExpression", "Eval");

        // Session lifecycle
        _disconnectMethod = FindMethod(_sessionType, flags, "Disconnect", "Close", "Detach", "Stop");
        _attachMethod = FindMethod(_sessionType, flags, "Attach", "Connect", "Start", "Initialize");
        _waitForBreakpointMethod = FindMethod(_sessionType, flags, "WaitForBreakpoint", "WaitForStop", "WaitForPause");

        Console.Error.WriteLine($"Discovered methods:");
        Console.Error.WriteLine($"  Continue:       {_continueMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  StepInto:       {_stepIntoMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  StepOut:        {_stepOutMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  StepOver:       {_stepOverMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  GetThreads:     {_getThreadsMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  GetStackTrace:  {_getStackTraceMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  GetScopes:      {_getScopesMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  GetVariables:   {_getVariablesMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  SetBreakpoints: {_setBreakpointsMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  AddBreakpoint:  {_addBreakpointMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  ClearBPs:       {_clearBreakpointsMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  Evaluate:       {_evaluateMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  Disconnect:     {_disconnectMethod?.Name ?? "NOT FOUND"}");
        Console.Error.WriteLine($"  Attach:         {_attachMethod?.Name ?? "NOT FOUND"}");
    }

    /// Discover related types (breakpoint, stackframe, variable).
    static void DiscoverRelatedTypes()
    {
        if (_debugAssembly == null) return;

        var allTypes = _debugAssembly.GetExportedTypes();

        // Breakpoint type
        _breakpointType = allTypes.FirstOrDefault(t =>
            t.Name.Contains("Breakpoint", StringComparison.OrdinalIgnoreCase) &&
            t.IsClass && !t.IsAbstract &&
            !t.Name.Contains("Collection", StringComparison.OrdinalIgnoreCase) &&
            !t.Name.Contains("Manager", StringComparison.OrdinalIgnoreCase));

        if (_breakpointType != null)
        {
            Console.Error.WriteLine($"Found breakpoint type: {_breakpointType.FullName}");
            DumpTypeMembers(_breakpointType);
        }

        // StackFrame type
        _stackFrameType = allTypes.FirstOrDefault(t =>
            (t.Name.Contains("StackFrame", StringComparison.OrdinalIgnoreCase) ||
             t.Name.Contains("CallFrame", StringComparison.OrdinalIgnoreCase)) &&
            t.IsClass && !t.IsAbstract);

        if (_stackFrameType != null)
        {
            Console.Error.WriteLine($"Found stack frame type: {_stackFrameType.FullName}");
            DumpTypeMembers(_stackFrameType);
        }

        // Variable type
        _variableType = allTypes.FirstOrDefault(t =>
            t.Name.Contains("Variable", StringComparison.OrdinalIgnoreCase) &&
            t.IsClass && !t.IsAbstract &&
            !t.Name.Contains("Collection", StringComparison.OrdinalIgnoreCase) &&
            !t.Name.Contains("Manager", StringComparison.OrdinalIgnoreCase));

        if (_variableType != null)
        {
            Console.Error.WriteLine($"Found variable type: {_variableType.FullName}");
            DumpTypeMembers(_variableType);
        }
    }

    // -----------------------------------------------------------------------
    // Debug Session Creation
    // -----------------------------------------------------------------------

    /// Create and connect a debug session using discovered types.
    static object? CreateDebugSession(string server, string serverInstance, string tenant,
        string authentication, string breakOnError)
    {
        if (_sessionType == null) return null;

        Console.Error.WriteLine($"Attempting to create session of type: {_sessionType.FullName}");

        // Dump available constructors
        var ctors = _sessionType.GetConstructors(BindingFlags.Public | BindingFlags.Instance);
        Console.Error.WriteLine($"Available constructors ({ctors.Length}):");
        foreach (var c in ctors)
        {
            var ps = string.Join(", ", c.GetParameters().Select(p => $"{p.ParameterType.Name} {p.Name}"));
            Console.Error.WriteLine($"  ctor({ps})");
        }

        // Also check for static factory methods
        var factories = _sessionType.GetMethods(BindingFlags.Public | BindingFlags.Static)
            .Where(m => m.ReturnType == _sessionType || m.ReturnType.IsAssignableTo(_sessionType))
            .ToList();

        if (factories.Count > 0)
        {
            Console.Error.WriteLine($"Available factory methods ({factories.Count}):");
            foreach (var f in factories)
            {
                var ps = string.Join(", ", f.GetParameters().Select(p => $"{p.ParameterType.Name} {p.Name}"));
                Console.Error.WriteLine($"  static {f.ReturnType.Name} {f.Name}({ps})");
            }
        }

        // Build the server URL — BC debugging typically uses OData/SOAP endpoint
        var serverUrl = server.TrimEnd('/');

        // Strategy 1: Try constructor with individual parameters
        foreach (var ctor in ctors)
        {
            var parameters = ctor.GetParameters();
            var args = TryMatchConstructorArgs(parameters, serverUrl, serverInstance, tenant, authentication);
            if (args != null)
            {
                Console.Error.WriteLine($"Trying constructor with {parameters.Length} params...");
                try
                {
                    var session = ctor.Invoke(args);
                    Console.Error.WriteLine($"Session created via constructor: {session.GetType().FullName}");
                    return session;
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine($"Constructor failed: {(ex.InnerException ?? ex).Message}");
                }
            }
        }

        // Strategy 2: Try parameterless constructor + property/method-based configuration
        var defaultCtor = ctors.FirstOrDefault(c => c.GetParameters().Length == 0);
        if (defaultCtor != null)
        {
            Console.Error.WriteLine("Trying parameterless constructor + property configuration...");
            try
            {
                var session = defaultCtor.Invoke(null);

                // Try to set connection properties
                SetPropertyIfExists(session, "Server", serverUrl);
                SetPropertyIfExists(session, "ServerUrl", serverUrl);
                SetPropertyIfExists(session, "ServerInstance", serverInstance);
                SetPropertyIfExists(session, "Instance", serverInstance);
                SetPropertyIfExists(session, "Tenant", tenant);
                SetPropertyIfExists(session, "TenantId", tenant);
                SetPropertyIfExists(session, "Authentication", authentication);
                SetPropertyIfExists(session, "AuthenticationType", authentication);

                // Try enum-based auth type
                TrySetAuthEnum(session, authentication);

                // Try to set break-on-error
                SetPropertyIfExists(session, "BreakOnError", breakOnError);
                SetPropertyIfExists(session, "BreakpointOnError", breakOnError);

                Console.Error.WriteLine($"Session created via default constructor with properties set");
                return session;
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Default constructor approach failed: {(ex.InnerException ?? ex).Message}");
            }
        }

        // Strategy 3: Try factory methods
        foreach (var factory in factories)
        {
            var parameters = factory.GetParameters();
            var args = TryMatchConstructorArgs(parameters, serverUrl, serverInstance, tenant, authentication);
            if (args != null)
            {
                Console.Error.WriteLine($"Trying factory method {factory.Name} with {parameters.Length} params...");
                try
                {
                    var session = factory.Invoke(null, args);
                    Console.Error.WriteLine($"Session created via factory: {factory.Name}");
                    return session;
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine($"Factory method {factory.Name} failed: {(ex.InnerException ?? ex).Message}");
                }
            }
        }

        // Strategy 4: Try single-param constructors that might take a config/options object
        foreach (var ctor in ctors.Where(c => c.GetParameters().Length == 1))
        {
            var paramType = ctor.GetParameters()[0].ParameterType;
            if (paramType.IsClass && !paramType.IsPrimitive && paramType != typeof(string))
            {
                Console.Error.WriteLine($"Trying constructor with config object of type {paramType.FullName}...");
                try
                {
                    var configObj = CreateConfigObject(paramType, serverUrl, serverInstance, tenant, authentication);
                    if (configObj != null)
                    {
                        var session = ctor.Invoke(new[] { configObj });
                        Console.Error.WriteLine($"Session created via config object constructor");
                        return session;
                    }
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine($"Config object constructor failed: {(ex.InnerException ?? ex).Message}");
                }
            }
        }

        Console.Error.WriteLine("All session creation strategies exhausted");
        return null;
    }

    /// Attempt to create a config/options object with connection properties.
    static object? CreateConfigObject(Type configType, string server, string instance, string tenant, string auth)
    {
        try
        {
            var configCtors = configType.GetConstructors(BindingFlags.Public | BindingFlags.Instance);
            var defaultCtor = configCtors.FirstOrDefault(c => c.GetParameters().Length == 0);
            if (defaultCtor == null) return null;

            var config = defaultCtor.Invoke(null);

            SetPropertyIfExists(config, "Server", server);
            SetPropertyIfExists(config, "ServerUrl", server);
            SetPropertyIfExists(config, "ServerInstance", instance);
            SetPropertyIfExists(config, "Instance", instance);
            SetPropertyIfExists(config, "Tenant", tenant);
            SetPropertyIfExists(config, "TenantId", tenant);
            SetPropertyIfExists(config, "Authentication", auth);
            SetPropertyIfExists(config, "AuthenticationType", auth);

            return config;
        }
        catch
        {
            return null;
        }
    }

    /// Try to attach the debugger after session creation.
    static void AttachDebugger()
    {
        if (_debugSession == null) return;

        try
        {
            if (_attachMethod != null)
            {
                var attachParams = _attachMethod.GetParameters();
                if (attachParams.Length == 0)
                {
                    _attachMethod.Invoke(_debugSession, null);
                    Console.Error.WriteLine("Debugger attached via Attach()");
                }
                else
                {
                    Console.Error.WriteLine($"Attach method requires {attachParams.Length} params — skipping auto-attach");
                    var ps = string.Join(", ", attachParams.Select(p => $"{p.ParameterType.Name} {p.Name}"));
                    Console.Error.WriteLine($"  Params: ({ps})");
                }
            }
            else
            {
                Console.Error.WriteLine("No Attach method found — session may already be connected");
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Warning: Attach failed (non-fatal): {(ex.InnerException ?? ex).Message}");
        }
    }

    // -----------------------------------------------------------------------
    // Breakpoint Operations via Reflection
    // -----------------------------------------------------------------------

    /// Set a single breakpoint via the debug session and return status.
    static (bool verified, int actualLine, string? message) SetBreakpointViaReflection(
        string file, int line, string? condition, string? hitCondition, string? logMessage, long bpId)
    {
        if (_debugSession == null)
            return (false, line, "No active debug session");

        // Strategy 1: Use AddBreakpoint(file, line) or similar
        if (_addBreakpointMethod != null)
        {
            try
            {
                var addParams = _addBreakpointMethod.GetParameters();
                object? bpResult = null;

                if (addParams.Length >= 2)
                {
                    // Try AddBreakpoint(string file, int line) variants
                    var args = TryMatchBreakpointArgs(addParams, file, line, condition);
                    if (args != null)
                    {
                        bpResult = _addBreakpointMethod.Invoke(_debugSession, args);
                    }
                }
                else if (addParams.Length == 1 && _breakpointType != null)
                {
                    // Takes a breakpoint object
                    var bpObj = CreateBreakpointObject(file, line, condition);
                    if (bpObj != null)
                    {
                        bpResult = _addBreakpointMethod.Invoke(_debugSession, new[] { bpObj });
                    }
                }

                if (bpResult != null)
                {
                    _breakpointMap[bpId] = bpResult;

                    // Try to read the actual verified line from the result
                    var resultLine = GetPropertyValue<int>(bpResult, "Line", "LineNumber") ?? line;
                    var verified = GetPropertyValue<bool>(bpResult, "Verified", "IsVerified", "IsValid") ?? true;
                    var msg = GetPropertyValue<string>(bpResult, "Message", "ErrorMessage");

                    return (verified, resultLine, msg);
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"AddBreakpoint failed: {(ex.InnerException ?? ex).Message}");
            }
        }

        // Strategy 2: Use SetBreakpoints with a collection
        if (_setBreakpointsMethod != null)
        {
            try
            {
                // This might take a file + array of lines
                var setParams = _setBreakpointsMethod.GetParameters();
                Console.Error.WriteLine($"SetBreakpoints params: {string.Join(", ", setParams.Select(p => $"{p.ParameterType.Name} {p.Name}"))}");
                // Attempt is best-effort — exact signature varies by BC version
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"SetBreakpoints approach failed: {(ex.InnerException ?? ex).Message}");
            }
        }

        // Strategy 3: Try direct property-based approaches
        try
        {
            // Some BC versions have a Breakpoints collection property
            var bpCollProp = _sessionType?.GetProperty("Breakpoints",
                BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);

            if (bpCollProp != null)
            {
                var collection = bpCollProp.GetValue(_debugSession);
                if (collection != null)
                {
                    // Try to call Add on the collection
                    var addMethod = collection.GetType().GetMethod("Add",
                        BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);

                    if (addMethod != null && _breakpointType != null)
                    {
                        var bpObj = CreateBreakpointObject(file, line, condition);
                        if (bpObj != null)
                        {
                            addMethod.Invoke(collection, new[] { bpObj });
                            _breakpointMap[bpId] = bpObj;
                            return (true, line, null);
                        }
                    }
                }
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Breakpoints collection approach failed: {ex.Message}");
        }

        // Fallback: report unverified
        return (false, line, "Could not set breakpoint via debug session API");
    }

    /// Clear breakpoints for a specific file.
    static void ClearBreakpointsForFile(string file)
    {
        if (_debugSession == null) return;

        // Strategy 1: Use ClearBreakpoints method
        if (_clearBreakpointsMethod != null)
        {
            try
            {
                var cParams = _clearBreakpointsMethod.GetParameters();
                if (cParams.Length == 0)
                {
                    // Clears all — not ideal but might be the only option
                    _clearBreakpointsMethod.Invoke(_debugSession, null);
                }
                else if (cParams.Length == 1 && cParams[0].ParameterType == typeof(string))
                {
                    _clearBreakpointsMethod.Invoke(_debugSession, new object[] { file });
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"ClearBreakpoints failed: {ex.Message}");
            }
        }

        // Strategy 2: Remove individual breakpoints we tracked
        var toRemove = _breakpointMap.ToList();
        foreach (var (id, bpObj) in toRemove)
        {
            // Check if this breakpoint is for the given file
            var bpFile = GetPropertyValue<string>(bpObj, "FileName", "FilePath", "File", "Source", "Path");
            if (bpFile != null && !string.Equals(bpFile, file, StringComparison.OrdinalIgnoreCase))
                continue;

            if (_removeBreakpointMethod != null)
            {
                try
                {
                    var rParams = _removeBreakpointMethod.GetParameters();
                    if (rParams.Length == 1)
                    {
                        if (rParams[0].ParameterType == typeof(int) || rParams[0].ParameterType == typeof(long))
                            _removeBreakpointMethod.Invoke(_debugSession, new object[] { id });
                        else
                            _removeBreakpointMethod.Invoke(_debugSession, new[] { bpObj });
                    }
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine($"RemoveBreakpoint {id} failed: {ex.Message}");
                }
            }

            _breakpointMap.Remove(id);
        }
    }

    /// Create a breakpoint object via reflection.
    static object? CreateBreakpointObject(string file, int line, string? condition)
    {
        if (_breakpointType == null) return null;

        try
        {
            var ctors = _breakpointType.GetConstructors(BindingFlags.Public | BindingFlags.Instance);

            // Try constructor with file + line
            foreach (var ctor in ctors)
            {
                var ps = ctor.GetParameters();
                if (ps.Length >= 2 &&
                    ps[0].ParameterType == typeof(string) &&
                    (ps[1].ParameterType == typeof(int) || ps[1].ParameterType == typeof(long)))
                {
                    var args = new List<object?> { file, ps[1].ParameterType == typeof(long) ? (long)line : line };
                    // Fill remaining optional params with defaults
                    for (int i = 2; i < ps.Length; i++)
                    {
                        if (ps[i].HasDefaultValue)
                            args.Add(ps[i].DefaultValue);
                        else if (ps[i].ParameterType == typeof(string))
                            args.Add(condition ?? "");
                        else
                            args.Add(ps[i].ParameterType.IsValueType ? Activator.CreateInstance(ps[i].ParameterType) : null);
                    }

                    return ctor.Invoke(args.ToArray());
                }
            }

            // Try default constructor + properties
            var defaultCtor = ctors.FirstOrDefault(c => c.GetParameters().Length == 0);
            if (defaultCtor != null)
            {
                var bp = defaultCtor.Invoke(null);
                SetPropertyIfExists(bp, "FileName", file);
                SetPropertyIfExists(bp, "FilePath", file);
                SetPropertyIfExists(bp, "File", file);
                SetPropertyIfExists(bp, "Line", line);
                SetPropertyIfExists(bp, "LineNumber", line);
                if (condition != null)
                    SetPropertyIfExists(bp, "Condition", condition);
                return bp;
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Failed to create breakpoint object: {ex.Message}");
        }

        return null;
    }

    // -----------------------------------------------------------------------
    // Thread/StackTrace/Variables via Reflection
    // -----------------------------------------------------------------------

    /// Get active threads from the debug session.
    static List<(long id, string name)> GetThreadsViaReflection()
    {
        var result = new List<(long id, string name)>();
        if (_debugSession == null) return result;

        // Strategy 1: Use GetThreads method
        if (_getThreadsMethod != null)
        {
            try
            {
                var threads = _getThreadsMethod.Invoke(_debugSession, null);
                if (threads != null)
                {
                    result = ExtractCollection(threads, obj =>
                    {
                        var id = GetPropertyValue<long>(obj, "Id", "ThreadId", "SessionId") ?? 0;
                        var name = GetPropertyValue<string>(obj, "Name", "ThreadName", "SessionName", "Description") ?? $"Thread {id}";
                        return (id, name);
                    });
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"GetThreads failed: {ex.Message}");
            }
        }

        // Strategy 2: Check for a Threads property
        if (result.Count == 0)
        {
            try
            {
                var threadsProp = _sessionType?.GetProperty("Threads",
                    BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);

                if (threadsProp != null)
                {
                    var threads = threadsProp.GetValue(_debugSession);
                    if (threads != null)
                    {
                        result = ExtractCollection(threads, obj =>
                        {
                            var id = GetPropertyValue<long>(obj, "Id", "ThreadId", "SessionId") ?? 0;
                            var name = GetPropertyValue<string>(obj, "Name", "ThreadName", "Description") ?? $"Thread {id}";
                            return (id, name);
                        });
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Threads property access failed: {ex.Message}");
            }
        }

        return result;
    }

    /// Get the stack trace for a thread.
    static List<(long id, string name, int line, int column, string? sourcePath)> GetStackTraceViaReflection(ulong threadId)
    {
        var result = new List<(long id, string name, int line, int column, string? sourcePath)>();
        if (_debugSession == null) return result;

        long frameIdCounter = 0;

        // Strategy 1: Use GetStackTrace(threadId) method
        if (_getStackTraceMethod != null)
        {
            try
            {
                object? stackResult;
                var stParams = _getStackTraceMethod.GetParameters();
                if (stParams.Length == 0)
                {
                    stackResult = _getStackTraceMethod.Invoke(_debugSession, null);
                }
                else if (stParams.Length == 1)
                {
                    var argType = stParams[0].ParameterType;
                    object arg = argType == typeof(long) ? (long)threadId :
                                 argType == typeof(int) ? (int)threadId :
                                 (object)(ulong)threadId;
                    stackResult = _getStackTraceMethod.Invoke(_debugSession, new[] { arg });
                }
                else
                {
                    stackResult = null;
                }

                if (stackResult != null)
                {
                    result = ExtractCollection(stackResult, obj =>
                    {
                        var id = GetPropertyValue<long>(obj, "Id", "FrameId", "Index") ?? frameIdCounter++;
                        var name = GetPropertyValue<string>(obj, "Name", "FunctionName", "MethodName", "ProcedureName")
                                   ?? GetPropertyValue<string>(obj, "ObjectName", "CodeunitName") ?? "Unknown";
                        var line = (int)(GetPropertyValue<long>(obj, "Line", "LineNumber", "LineNo") ?? 0);
                        var column = (int)(GetPropertyValue<long>(obj, "Column", "ColumnNumber") ?? 0);
                        var source = GetPropertyValue<string>(obj, "FileName", "FilePath", "Source", "SourceFile", "Path");
                        return (id, name, line, column, source);
                    });
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"GetStackTrace failed: {ex.Message}");
            }
        }

        // Strategy 2: Check for a StackTrace/CallStack property
        if (result.Count == 0)
        {
            try
            {
                var stProp = _sessionType?.GetProperty("StackTrace",
                    BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase)
                    ?? _sessionType?.GetProperty("CallStack",
                        BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);

                if (stProp != null)
                {
                    var stack = stProp.GetValue(_debugSession);
                    if (stack != null)
                    {
                        result = ExtractCollection(stack, obj =>
                        {
                            var id = GetPropertyValue<long>(obj, "Id", "FrameId") ?? frameIdCounter++;
                            var name = GetPropertyValue<string>(obj, "Name", "FunctionName", "MethodName") ?? "Unknown";
                            var line = (int)(GetPropertyValue<long>(obj, "Line", "LineNumber") ?? 0);
                            var column = (int)(GetPropertyValue<long>(obj, "Column") ?? 0);
                            var source = GetPropertyValue<string>(obj, "FileName", "FilePath", "Source");
                            return (id, name, line, column, source);
                        });
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"StackTrace property access failed: {ex.Message}");
            }
        }

        return result;
    }

    /// Get scopes for a stack frame.
    static List<(string name, long varRef, bool expensive)> GetScopesViaReflection(long frameId)
    {
        var result = new List<(string name, long varRef, bool expensive)>();
        if (_debugSession == null) return result;

        // Strategy 1: Use GetScopes method
        if (_getScopesMethod != null)
        {
            try
            {
                var sParams = _getScopesMethod.GetParameters();
                object? scopesResult;

                if (sParams.Length == 0)
                    scopesResult = _getScopesMethod.Invoke(_debugSession, null);
                else if (sParams.Length == 1)
                {
                    var argType = sParams[0].ParameterType;
                    object arg = argType == typeof(long) ? frameId :
                                 argType == typeof(int) ? (int)frameId :
                                 (object)(ulong)frameId;
                    scopesResult = _getScopesMethod.Invoke(_debugSession, new[] { arg });
                }
                else
                    scopesResult = null;

                if (scopesResult != null)
                {
                    result = ExtractCollection(scopesResult, obj =>
                    {
                        var name = GetPropertyValue<string>(obj, "Name", "ScopeName") ?? "Variables";
                        var expensive = GetPropertyValue<bool>(obj, "Expensive") ?? false;

                        // Allocate a variablesReference and track what scope it maps to
                        var varRef = _nextVariableRef++;
                        _variableRefMap[varRef] = (frameId, name);
                        _containerMap[varRef] = obj; // Store the scope object for variable lookup

                        return (name, varRef, expensive);
                    });
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"GetScopes failed: {ex.Message}");
            }
        }

        // If we got no scopes from the API, create standard ones
        if (result.Count == 0)
        {
            var localRef = _nextVariableRef++;
            var globalRef = _nextVariableRef++;
            _variableRefMap[localRef] = (frameId, "Locals");
            _variableRefMap[globalRef] = (frameId, "Globals");
            result.Add(("Locals", localRef, false));
            result.Add(("Globals", globalRef, false));
        }

        return result;
    }

    /// Get variables for a variablesReference.
    static List<(string name, string value, string? type, long childRef)> GetVariablesViaReflection(long reference)
    {
        var result = new List<(string name, string value, string? type, long childRef)>();
        if (_debugSession == null) return result;

        // Check if this reference maps to a scope we know about
        _variableRefMap.TryGetValue(reference, out var scopeInfo);
        _containerMap.TryGetValue(reference, out var container);

        // Strategy 1: Use GetVariables method on the session
        if (_getVariablesMethod != null)
        {
            try
            {
                var vParams = _getVariablesMethod.GetParameters();
                object? varsResult = null;

                if (vParams.Length == 0)
                {
                    varsResult = _getVariablesMethod.Invoke(_debugSession, null);
                }
                else if (vParams.Length == 1)
                {
                    // Could be frameId, scopeId, or variablesReference
                    var argType = vParams[0].ParameterType;
                    if (argType == typeof(long) || argType == typeof(int) || argType == typeof(ulong))
                    {
                        object arg = argType == typeof(long) ? scopeInfo.frameId :
                                     argType == typeof(int) ? (int)scopeInfo.frameId :
                                     (object)(ulong)scopeInfo.frameId;
                        varsResult = _getVariablesMethod.Invoke(_debugSession, new[] { arg });
                    }
                    else if (container != null && argType.IsAssignableFrom(container.GetType()))
                    {
                        varsResult = _getVariablesMethod.Invoke(_debugSession, new[] { container });
                    }
                }
                else if (vParams.Length == 2)
                {
                    // Might be (frameId, scopeName) or similar
                    var args = new object?[2];
                    for (int i = 0; i < 2; i++)
                    {
                        var pType = vParams[i].ParameterType;
                        if (pType == typeof(string))
                            args[i] = scopeInfo.scope;
                        else if (pType == typeof(long))
                            args[i] = scopeInfo.frameId;
                        else if (pType == typeof(int))
                            args[i] = (int)scopeInfo.frameId;
                        else if (pType == typeof(ulong))
                            args[i] = (ulong)scopeInfo.frameId;
                        else
                            args[i] = null;
                    }
                    varsResult = _getVariablesMethod.Invoke(_debugSession, args);
                }

                if (varsResult != null)
                {
                    result = ExtractCollection(varsResult, obj =>
                    {
                        var name = GetPropertyValue<string>(obj, "Name", "VariableName") ?? "?";
                        var value = GetPropertyValue<string>(obj, "Value", "TextValue", "DisplayValue")
                                    ?? GetPropertyValue<object>(obj, "Value")?.ToString() ?? "";
                        var type = GetPropertyValue<string>(obj, "Type", "TypeName", "DataType", "VariableType");

                        // Check if this variable has children (for records, arrays, etc.)
                        long childRef = 0;
                        var hasChildren = GetPropertyValue<bool>(obj, "HasChildren", "HasMembers", "IsComplex") ?? false;
                        if (!hasChildren)
                        {
                            // Also check for a Children/Members property
                            var childProp = obj.GetType().GetProperty("Children",
                                BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase)
                                ?? obj.GetType().GetProperty("Members",
                                    BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);
                            hasChildren = childProp != null;
                        }

                        if (hasChildren)
                        {
                            childRef = _nextVariableRef++;
                            _containerMap[childRef] = obj;
                            _variableRefMap[childRef] = (scopeInfo.frameId, name);
                        }

                        return (name, value, type, childRef);
                    });
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"GetVariables failed: {ex.Message}");
            }
        }

        // Strategy 2: If container has a Children/Members/Variables property, enumerate that
        if (result.Count == 0 && container != null)
        {
            try
            {
                var childProp = container.GetType().GetProperty("Children",
                    BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase)
                    ?? container.GetType().GetProperty("Members",
                        BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase)
                    ?? container.GetType().GetProperty("Variables",
                        BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);

                if (childProp != null)
                {
                    var children = childProp.GetValue(container);
                    if (children != null)
                    {
                        result = ExtractCollection(children, obj =>
                        {
                            var name = GetPropertyValue<string>(obj, "Name", "VariableName") ?? "?";
                            var value = GetPropertyValue<string>(obj, "Value", "TextValue")
                                        ?? GetPropertyValue<object>(obj, "Value")?.ToString() ?? "";
                            var type = GetPropertyValue<string>(obj, "Type", "TypeName", "DataType");

                            long childRef = 0;
                            var hasChildren = GetPropertyValue<bool>(obj, "HasChildren", "HasMembers") ?? false;
                            if (hasChildren)
                            {
                                childRef = _nextVariableRef++;
                                _containerMap[childRef] = obj;
                                _variableRefMap[childRef] = (scopeInfo.frameId, name);
                            }

                            return (name, value, type, childRef);
                        });
                    }
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Container children extraction failed: {ex.Message}");
            }
        }

        return result;
    }

    /// Evaluate an expression in the debug context.
    static (string result, string? type) EvaluateViaReflection(string expression, long? frameId)
    {
        if (_debugSession == null)
            return ("<not connected>", null);

        // Strategy 1: Use Evaluate method
        if (_evaluateMethod != null)
        {
            var eParams = _evaluateMethod.GetParameters();
            object? evalResult;

            if (eParams.Length == 1 && eParams[0].ParameterType == typeof(string))
            {
                evalResult = _evaluateMethod.Invoke(_debugSession, new object[] { expression });
            }
            else if (eParams.Length == 2)
            {
                var args = new object?[2];
                for (int i = 0; i < 2; i++)
                {
                    if (eParams[i].ParameterType == typeof(string))
                        args[i] = expression;
                    else if (eParams[i].ParameterType == typeof(long))
                        args[i] = frameId ?? 0L;
                    else if (eParams[i].ParameterType == typeof(int))
                        args[i] = (int)(frameId ?? 0);
                    else
                        args[i] = null;
                }
                evalResult = _evaluateMethod.Invoke(_debugSession, args);
            }
            else if (eParams.Length == 0)
            {
                // Unlikely but handle it
                evalResult = _evaluateMethod.Invoke(_debugSession, null);
            }
            else
            {
                return ($"<eval not supported: Evaluate takes {eParams.Length} params>", null);
            }

            if (evalResult == null)
                return ("null", null);

            // The result could be a string, or a typed object with Value/Type properties
            if (evalResult is string str)
                return (str, null);

            var resultValue = GetPropertyValue<string>(evalResult, "Value", "Result", "TextValue")
                              ?? evalResult.ToString() ?? "";
            var resultType = GetPropertyValue<string>(evalResult, "Type", "TypeName", "DataType");

            return (resultValue, resultType);
        }

        // Strategy 2: Try calling Evaluate via dynamic dispatch
        try
        {
            dynamic session = _debugSession;
            var evalResult = session.Evaluate(expression);
            return (evalResult?.ToString() ?? "null", null);
        }
        catch (Microsoft.CSharp.RuntimeBinder.RuntimeBinderException)
        {
            return ($"<evaluation not supported by this debug session>", null);
        }
    }

    // -----------------------------------------------------------------------
    // Reflection Helpers
    // -----------------------------------------------------------------------

    /// Find the first method matching any of the given names.
    static MethodInfo? FindMethod(Type type, BindingFlags flags, params string[] names)
    {
        foreach (var name in names)
        {
            // Try exact match first (case-insensitive due to flags)
            var method = type.GetMethod(name, flags);
            if (method != null) return method;
        }

        // Fallback: search all methods for partial match
        var methods = type.GetMethods(flags);
        foreach (var name in names)
        {
            var match = methods.FirstOrDefault(m =>
                m.Name.Equals(name, StringComparison.OrdinalIgnoreCase));
            if (match != null) return match;
        }

        return null;
    }

    /// Try to invoke a parameterless method by name. Returns true if invoked.
    static bool TryInvoke(object target, string methodName)
    {
        try
        {
            var method = target.GetType().GetMethod(methodName,
                BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase,
                null, Type.EmptyTypes, null);
            if (method != null)
            {
                method.Invoke(target, null);
                Console.Error.WriteLine($"Invoked {methodName}()");
                return true;
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"TryInvoke({methodName}) failed: {ex.Message}");
        }
        return false;
    }

    /// Get a property value from an object, trying multiple names.
    static T? GetPropertyValue<T>(object obj, params string[] names)
    {
        var flags = BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase;
        foreach (var name in names)
        {
            try
            {
                var prop = obj.GetType().GetProperty(name, flags);
                if (prop != null)
                {
                    var val = prop.GetValue(obj);
                    if (val is T typed) return typed;
                    if (val != null)
                    {
                        // Try conversion
                        try { return (T)Convert.ChangeType(val, typeof(T)); }
                        catch { /* ignore conversion failures */ }
                    }
                }
            }
            catch { /* ignore */ }
        }
        return default;
    }

    /// Set a property on an object if it exists.
    static void SetPropertyIfExists(object obj, string name, object? value)
    {
        try
        {
            var prop = obj.GetType().GetProperty(name,
                BindingFlags.Public | BindingFlags.Instance | BindingFlags.IgnoreCase);
            if (prop != null && prop.CanWrite)
            {
                // Try to convert value to the property type
                var targetType = prop.PropertyType;
                object? convertedValue = value;

                if (value is string strVal && targetType != typeof(string))
                {
                    // Try parsing enums
                    if (targetType.IsEnum)
                    {
                        convertedValue = Enum.Parse(targetType, strVal, ignoreCase: true);
                    }
                    else if (targetType == typeof(int))
                    {
                        convertedValue = int.Parse(strVal);
                    }
                    else if (targetType == typeof(Uri))
                    {
                        convertedValue = new Uri(strVal);
                    }
                }
                else if (value is int intVal && targetType == typeof(long))
                {
                    convertedValue = (long)intVal;
                }

                prop.SetValue(obj, convertedValue);
                Console.Error.WriteLine($"Set property {name} = {value}");
            }
        }
        catch (Exception ex)
        {
            Console.Error.WriteLine($"Could not set property {name}: {ex.Message}");
        }
    }

    /// Try to set authentication enum on the session object.
    static void TrySetAuthEnum(object session, string authString)
    {
        if (_debugAssembly == null) return;

        // Look for authentication-related enum types
        var authEnumTypes = _debugAssembly.GetExportedTypes()
            .Where(t => t.IsEnum && (
                t.Name.Contains("Auth", StringComparison.OrdinalIgnoreCase) ||
                t.Name.Contains("Credential", StringComparison.OrdinalIgnoreCase)))
            .ToList();

        foreach (var enumType in authEnumTypes)
        {
            try
            {
                var enumValue = Enum.Parse(enumType, authString, ignoreCase: true);
                // Try to set it on matching properties
                var authProps = session.GetType().GetProperties(BindingFlags.Public | BindingFlags.Instance)
                    .Where(p => p.PropertyType == enumType && p.CanWrite);
                foreach (var prop in authProps)
                {
                    prop.SetValue(session, enumValue);
                    Console.Error.WriteLine($"Set auth enum {prop.Name} = {enumValue} (type {enumType.Name})");
                }
            }
            catch { /* not all enum types will parse the given string */ }
        }
    }

    /// Try to match constructor parameters to our connection arguments.
    static object?[]? TryMatchConstructorArgs(ParameterInfo[] parameters, string server, string instance,
        string tenant, string auth)
    {
        var args = new object?[parameters.Length];
        var matched = new bool[parameters.Length];

        // Map parameter names to values
        var nameMap = new Dictionary<string, object>(StringComparer.OrdinalIgnoreCase)
        {
            ["server"] = server,
            ["serverUrl"] = server,
            ["url"] = server,
            ["endpoint"] = server,
            ["serverInstance"] = instance,
            ["instance"] = instance,
            ["instanceName"] = instance,
            ["tenant"] = tenant,
            ["tenantId"] = tenant,
            ["authentication"] = auth,
            ["authenticationType"] = auth,
            ["authType"] = auth,
        };

        for (int i = 0; i < parameters.Length; i++)
        {
            var p = parameters[i];

            // Try to match by parameter name
            if (p.Name != null && nameMap.TryGetValue(p.Name, out var value))
            {
                if (p.ParameterType == typeof(string))
                {
                    args[i] = value;
                    matched[i] = true;
                }
                else if (p.ParameterType == typeof(Uri))
                {
                    args[i] = new Uri((string)value);
                    matched[i] = true;
                }
                else if (p.ParameterType.IsEnum && value is string sv)
                {
                    try
                    {
                        args[i] = Enum.Parse(p.ParameterType, sv, ignoreCase: true);
                        matched[i] = true;
                    }
                    catch { /* can't parse this enum value */ }
                }
            }

            // Fall back to default value
            if (!matched[i] && p.HasDefaultValue)
            {
                args[i] = p.DefaultValue;
                matched[i] = true;
            }

            // Give up on this constructor if we can't fill a required param
            if (!matched[i] && !p.HasDefaultValue)
            {
                return null;
            }
        }

        return args;
    }

    /// Try to match breakpoint method arguments.
    static object?[]? TryMatchBreakpointArgs(ParameterInfo[] parameters, string file, int line, string? condition)
    {
        var args = new object?[parameters.Length];

        for (int i = 0; i < parameters.Length; i++)
        {
            var p = parameters[i];
            var pName = p.Name?.ToLower() ?? "";

            if (p.ParameterType == typeof(string))
            {
                if (pName.Contains("file") || pName.Contains("path") || pName.Contains("source") || i == 0)
                    args[i] = file;
                else if (pName.Contains("condition"))
                    args[i] = condition ?? "";
                else if (p.HasDefaultValue)
                    args[i] = p.DefaultValue;
                else
                    args[i] = "";
            }
            else if (p.ParameterType == typeof(int))
            {
                args[i] = pName.Contains("line") || i == 1 ? line : 0;
            }
            else if (p.ParameterType == typeof(long))
            {
                args[i] = pName.Contains("line") || i == 1 ? (long)line : 0L;
            }
            else if (p.ParameterType == typeof(bool))
            {
                args[i] = true; // e.g., "enabled"
            }
            else if (p.HasDefaultValue)
            {
                args[i] = p.DefaultValue;
            }
            else
            {
                return null; // Can't fill this parameter
            }
        }

        return args;
    }

    /// Extract a list of items from a reflected collection (IEnumerable, Array, etc.)
    static List<T> ExtractCollection<T>(object collection, Func<object, T> extract)
    {
        var result = new List<T>();

        if (collection is System.Collections.IEnumerable enumerable)
        {
            foreach (var item in enumerable)
            {
                if (item == null) continue;
                try
                {
                    result.Add(extract(item));
                }
                catch (Exception ex)
                {
                    Console.Error.WriteLine($"Failed to extract item: {ex.Message}");
                }
            }
        }
        else
        {
            // Try to find a Count + indexer
            var countProp = collection.GetType().GetProperty("Count",
                BindingFlags.Public | BindingFlags.Instance);
            var itemProp = collection.GetType().GetProperty("Item",
                BindingFlags.Public | BindingFlags.Instance);

            if (countProp != null && itemProp != null)
            {
                var count = (int)(countProp.GetValue(collection) ?? 0);
                for (int i = 0; i < count; i++)
                {
                    try
                    {
                        var item = itemProp.GetValue(collection, new object[] { i });
                        if (item != null)
                            result.Add(extract(item));
                    }
                    catch (Exception ex)
                    {
                        Console.Error.WriteLine($"Failed to extract item [{i}]: {ex.Message}");
                    }
                }
            }
        }

        return result;
    }

    /// Dump type members for diagnostic logging.
    static void DumpTypeMembers(Type type)
    {
        var flags = BindingFlags.Public | BindingFlags.Instance;

        var props = type.GetProperties(flags);
        if (props.Length > 0)
        {
            Console.Error.WriteLine($"  Properties:");
            foreach (var p in props)
                Console.Error.WriteLine($"    {p.PropertyType.Name} {p.Name} {{ {(p.CanRead ? "get; " : "")}{(p.CanWrite ? "set; " : "")}}}");
        }

        var methods = type.GetMethods(flags).Where(m => !m.IsSpecialName).ToArray();
        if (methods.Length > 0)
        {
            Console.Error.WriteLine($"  Methods:");
            foreach (var m in methods)
            {
                var ps = string.Join(", ", m.GetParameters().Select(p => $"{p.ParameterType.Name} {p.Name}"));
                Console.Error.WriteLine($"    {m.ReturnType.Name} {m.Name}({ps})");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    static JsonNode MakeError(string message)
    {
        return new JsonObject { ["error"] = message };
    }
}
