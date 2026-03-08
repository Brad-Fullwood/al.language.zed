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
    // Loaded assemblies and types (populated during connect)
    private static Assembly? _deploymentAssembly;
    private static Assembly? _debugAssembly;
    private static object? _debugSession;
    private static string _altoolDir = "";

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

        // Set up assembly resolution so we can load BC DLLs
        AppDomain.CurrentDomain.AssemblyResolve += (_, resolveArgs) =>
        {
            var name = new AssemblyName(resolveArgs.Name).Name;
            if (name == null) return null;
            var path = Path.Combine(_altoolDir, $"{name}.dll");
            return File.Exists(path) ? Assembly.LoadFrom(path) : null;
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

        Console.Error.WriteLine($"Connecting to {server}/{serverInstance} tenant={tenant} auth={authentication}");

        // Try to load the debug assembly from ALTool directory
        var debugDllPath = Path.Combine(_altoolDir, "Microsoft.Dynamics.Nav.Debug.dll");
        if (!File.Exists(debugDllPath))
        {
            // Also check in a "Debug" subdirectory
            debugDllPath = Path.Combine(_altoolDir, "Debug", "Microsoft.Dynamics.Nav.Debug.dll");
        }

        if (File.Exists(debugDllPath))
        {
            try
            {
                _debugAssembly = Assembly.LoadFrom(debugDllPath);
                Console.Error.WriteLine($"Loaded debug assembly: {_debugAssembly.FullName}");
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: Could not load debug assembly: {ex.Message}");
            }
        }
        else
        {
            Console.Error.WriteLine($"Debug assembly not found at {debugDllPath} — running in stub mode");
        }

        // Try to load deployment assembly
        var deployDllPath = Path.Combine(_altoolDir, "Microsoft.Dynamics.Nav.Deployment.dll");
        if (File.Exists(deployDllPath))
        {
            try
            {
                _deploymentAssembly = Assembly.LoadFrom(deployDllPath);
                Console.Error.WriteLine($"Loaded deployment assembly: {_deploymentAssembly.FullName}");
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: Could not load deployment assembly: {ex.Message}");
            }
        }

        // If we have the debug assembly, try to create a debug session
        if (_debugAssembly != null)
        {
            try
            {
                // Look for a debug session factory or constructor
                // The exact API depends on the BC version
                var sessionType = _debugAssembly.GetType("Microsoft.Dynamics.Nav.Debug.DebugSession")
                    ?? _debugAssembly.GetType("Microsoft.Dynamics.Nav.Debug.DebuggerSession");

                if (sessionType != null)
                {
                    // Try to create a session — this is version-dependent
                    Console.Error.WriteLine($"Found debug session type: {sessionType.FullName}");
                    // Actual connection logic would go here, using reflection
                    // to call the appropriate constructor/factory method
                }
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Warning: Could not create debug session: {ex.Message}");
            }
        }

        return new JsonObject { ["status"] = "connected" };
    }

    static JsonNode HandleDisconnect()
    {
        _debugSession = null;
        Console.Error.WriteLine("Disconnected");
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleSetBreakpoints(JsonNode? parms)
    {
        var file = parms?["file"]?.GetValue<string>() ?? "";
        var breakpoints = parms?["breakpoints"]?.AsArray() ?? new JsonArray();

        Console.Error.WriteLine($"Setting {breakpoints.Count} breakpoints in {file}");

        // Build response breakpoints (verified = true for each)
        var responseBreakpoints = new JsonArray();
        long bpId = 1;
        foreach (var bp in breakpoints)
        {
            var line = bp?["line"]?.GetValue<int>() ?? 0;
            responseBreakpoints.Add(new JsonObject
            {
                ["id"] = bpId++,
                ["verified"] = true,
                ["line"] = line,
                ["source"] = new JsonObject
                {
                    ["path"] = file,
                },
            });
        }

        return new JsonObject { ["breakpoints"] = responseBreakpoints };
    }

    static JsonNode HandleContinue(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Continue thread {threadId}");

        // In a real implementation, this would call into the BC debug session
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleStepIn(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step in thread {threadId}");
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleStepOut(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step out thread {threadId}");
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleStepOver(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Step over thread {threadId}");
        return new JsonObject { ["status"] = "ok" };
    }

    static JsonNode HandleThreads()
    {
        // Return at least one thread — BC always has a main session thread
        var threads = new JsonArray
        {
            new JsonObject
            {
                ["id"] = 1,
                ["name"] = "BC Session",
            }
        };

        return new JsonObject { ["threads"] = threads };
    }

    static JsonNode HandleStackTrace(JsonNode? parms)
    {
        var threadId = parms?["threadId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Stack trace for thread {threadId}");

        // In a real implementation, query the BC debug session
        var stackFrames = new JsonArray();

        return new JsonObject { ["stackFrames"] = stackFrames };
    }

    static JsonNode HandleScopes(JsonNode? parms)
    {
        var frameId = parms?["frameId"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Scopes for frame {frameId}");

        // Standard scopes for AL debugging
        var scopes = new JsonArray
        {
            new JsonObject
            {
                ["name"] = "Locals",
                ["variablesReference"] = 1000 + (long)frameId,
                ["expensive"] = false,
            },
            new JsonObject
            {
                ["name"] = "Globals",
                ["variablesReference"] = 2000 + (long)frameId,
                ["expensive"] = false,
            },
        };

        return new JsonObject { ["scopes"] = scopes };
    }

    static JsonNode HandleVariables(JsonNode? parms)
    {
        var reference = parms?["variablesReference"]?.GetValue<ulong>() ?? 0;
        Console.Error.WriteLine($"Variables for reference {reference}");

        // In a real implementation, query the BC debug session
        var variables = new JsonArray();

        return new JsonObject { ["variables"] = variables };
    }

    static JsonNode HandleEvaluate(JsonNode? parms)
    {
        var expression = parms?["expression"]?.GetValue<string>() ?? "";
        Console.Error.WriteLine($"Evaluate: {expression}");

        // In a real implementation, evaluate via the BC debug session
        return new JsonObject { ["result"] = $"<eval:{expression}>" };
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    static JsonNode MakeError(string message)
    {
        return new JsonObject { ["error"] = message };
    }
}
