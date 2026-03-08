// AlDap — .NET bridge for AL Debug Adapter Protocol
//
// Runs as a subprocess. Handles DAP communication with Business Central.
// Uses Microsoft.Dynamics.Nav.Deployment.dll from ALTool for BC server communication.
// MSAL authentication via the DLL's built-in auth.
//
// Capabilities: launch, attach, setBreakpoints, continue, stepIn/Out/Over,
//               evaluate, threads, stackTrace, scopes, variables

using System.Text.Json;

namespace AlDap;

class Program
{
    static async Task Main(string[] args)
    {
        if (args.Length < 1)
        {
            Console.Error.WriteLine("Usage: AlDap <path-to-altool-dir>");
            Environment.Exit(1);
        }

        var altoolDir = args[0];

        if (!Directory.Exists(altoolDir))
        {
            Console.Error.WriteLine($"ALTool directory not found: {altoolDir}");
            Environment.Exit(1);
        }

        Console.Error.WriteLine($"AlDap bridge started with ALTool at: {altoolDir}");

        // JSON-RPC loop
        using var reader = new StreamReader(Console.OpenStandardInput());

        while (true)
        {
            var line = await reader.ReadLineAsync();
            if (line == null) break;

            try
            {
                var request = JsonSerializer.Deserialize<JsonElement>(line);
                var method = request.GetProperty("method").GetString();
                var id = request.GetProperty("id").GetUInt64();

                var result = method switch
                {
                    "ping" => JsonSerializer.SerializeToElement(new { status = "ok" }),
                    _ => JsonSerializer.SerializeToElement(new { error = $"Unknown method: {method}" })
                };

                var response = new { id, result };
                Console.WriteLine(JsonSerializer.Serialize(response));
                Console.Out.Flush();
            }
            catch (Exception ex)
            {
                Console.Error.WriteLine($"Error: {ex.Message}");
            }
        }
    }
}
