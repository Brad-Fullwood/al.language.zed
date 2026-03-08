// AlSemantic — .NET bridge for AL CodeAnalysis API
//
// Runs as a subprocess. Reads JSON-RPC requests from stdin, writes responses to stdout.
// Loads CodeAnalysis.dll from the ALTool installation path (passed as first argument).
//
// Commands:
//   analyze    — run DiagnosticAnalyzers on source
//   compile    — invoke Compilation API
//   typeAt     — resolve symbol at position
//   completions — get completion items
//   builtins   — extract all built-in types and methods
//   errorCodes — list all compiler error codes
//   ping       — health check

using System.Reflection;
using System.Text.Json;

namespace AlSemantic;

class Program
{
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

        // JSON-RPC loop: read line-delimited JSON from stdin
        using var reader = new StreamReader(Console.OpenStandardInput());

        while (true)
        {
            var line = await reader.ReadLineAsync();
            if (line == null) break; // EOF

            try
            {
                var request = JsonSerializer.Deserialize<JsonElement>(line);
                var method = request.GetProperty("method").GetString();
                var id = request.GetProperty("id").GetUInt64();

                var result = method switch
                {
                    "ping" => HandlePing(),
                    "builtins" => HandleBuiltins(codeAnalysis!),
                    "errorCodes" => HandleErrorCodes(codeAnalysis!),
                    _ => JsonSerializer.SerializeToElement(new { error = $"Unknown method: {method}" })
                };

                var response = new { id, result };
                Console.WriteLine(JsonSerializer.Serialize(response));
                Console.Out.Flush();
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Error processing request: {ex.Message}");
            }
        }
    }

    static JsonElement HandlePing()
    {
        return JsonSerializer.SerializeToElement(new { status = "ok" });
    }

    static JsonElement HandleBuiltins(Assembly codeAnalysis)
    {
        // TODO: Extract built-in types from CodeAnalysis using reflection
        _ = codeAnalysis;
        return JsonSerializer.SerializeToElement(new object[] { });
    }

    static JsonElement HandleErrorCodes(Assembly codeAnalysis)
    {
        // TODO: Extract error codes from CodeAnalysis using reflection
        _ = codeAnalysis;
        return JsonSerializer.SerializeToElement(new object[] { });
    }
}
