// Extracts AL built-ins and runtime enums from Microsoft.Dynamics.Nav.CodeAnalysis.
// Usage: dotnet run -- [--output <dir>] [--dll <path>]

using System.Reflection;
using System.Text.Json;
using System.Text.Json.Serialization;

string? outputDir = null;
string? explicitDll = null;

for (int i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--output" when i + 1 < args.Length:
            outputDir = args[++i];
            break;
        case "--dll" when i + 1 < args.Length:
            explicitDll = args[++i];
            break;
        case "--output" or "--dll":
            Console.Error.WriteLine($"Missing value for {args[i]}");
            return 2;
        default:
            Console.Error.WriteLine($"Unknown argument: {args[i]}");
            return 2;
    }
}

if (outputDir == null)
{
    var exeDir = AppContext.BaseDirectory;
    outputDir = Path.GetFullPath(Path.Combine(exeDir, "..", "..", "..", "..", "..", "..", "data"));
}

outputDir = Path.GetFullPath(outputDir);
Console.Error.WriteLine($"Output directory: {outputDir}");
Directory.CreateDirectory(outputDir);

string? dllPath = explicitDll;

if (dllPath == null)
{
    var home = Environment.GetFolderPath(Environment.SpecialFolder.UserProfile);
    var searchRoots = new[]
    {
        Path.Combine(home, ".cursor", "extensions"),
        Path.Combine(home, ".vscode", "extensions"),
    };

    foreach (var root in searchRoots)
    {
        if (!Directory.Exists(root)) continue;
        var alDirs = Directory.GetDirectories(root, "ms-dynamics-smb.al-*")
            .Select(directory => (Directory: directory, Version: ExtensionVersion(directory)))
            .Where(candidate => candidate.Version != null)
            .OrderByDescending(candidate => candidate.Version)
            .Select(candidate => candidate.Directory)
            .ToList();
        foreach (var dir in alDirs)
        {
            var platform = Environment.OSVersion.Platform switch
            {
                PlatformID.Unix => "linux",
                PlatformID.Win32NT => "win32",
                _ => throw new PlatformNotSupportedException()
            };
            if (OperatingSystem.IsMacOS()) platform = "darwin";

            var candidates = new[]
            {
                Path.Combine(dir, "bin", platform, "Microsoft.Dynamics.Nav.CodeAnalysis.dll"),
                Path.Combine(dir, "bin", "Microsoft.Dynamics.Nav.CodeAnalysis.dll"),
            };
            foreach (var c in candidates)
            {
                if (File.Exists(c)) { dllPath = c; break; }
            }
            if (dllPath != null) break;
        }
        if (dllPath != null) break;
    }
}

if (dllPath == null)
{
    Console.Error.WriteLine("Error: Microsoft AL extension not found.");
    Console.Error.WriteLine("Searched:");
    Console.Error.WriteLine("  ~/.cursor/extensions/ms-dynamics-smb.al-*/bin/");
    Console.Error.WriteLine("  ~/.vscode/extensions/ms-dynamics-smb.al-*/bin/");
    Console.Error.WriteLine("Install the AL Language extension in VS Code or Cursor first.");
    Console.Error.WriteLine("Alternatively, pass --dll <path/to/Microsoft.Dynamics.Nav.CodeAnalysis.dll>");
    return 1;
}

Console.Error.WriteLine($"Using DLL: {dllPath}");
var binDir = Path.GetDirectoryName(dllPath)!;

// Deduplicate by filename — MetadataLoadContext rejects duplicate assembly identities.
// Runtime assemblies must come first so mscorlib/System.* resolve correctly.
var seenNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
var resolverPaths = new List<string>();

void AddResolverPath(string p)
{
    var name = Path.GetFileName(p);
    if (seenNames.Add(name)) resolverPaths.Add(p);
}

var runtimeDir = Path.GetDirectoryName(typeof(object).Assembly.Location)!;
foreach (var f in Directory.GetFiles(runtimeDir, "*.dll")) AddResolverPath(f);
AddResolverPath(dllPath);
foreach (var f in Directory.GetFiles(binDir, "*.dll")) AddResolverPath(f);

MetadataLoadContext mlc;
Assembly codeAnalysis;
try
{
    var resolver = new PathAssemblyResolver(resolverPaths);
    mlc = new MetadataLoadContext(resolver);
    codeAnalysis = mlc.LoadFromAssemblyPath(dllPath);
}
catch (Exception ex)
{
    Console.Error.WriteLine($"Error loading assembly: {ex.Message}");
    return 1;
}

var allTypes = codeAnalysis.GetTypes();
Console.Error.WriteLine($"Loaded {allTypes.Length} types from {Path.GetFileName(dllPath)}");

Console.Error.WriteLine("\n--- Extracting runtime enums ---");

var runtimeEnumNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase)
{
    "WebServiceActionResultCode",
    "SecurityFilter",
    "DataScope",
    "ErrorBehavior",
    "TestPermissions",
    "TransactionModel",
    "CommitBehavior",
    "InherentPermissionsScope",
};

var runtimeEnumResults = new List<object>();

foreach (var type in allTypes)
{
    if (!type.IsEnum) continue;
    if (type.DeclaringType?.Name != "SystemOptionKinds") continue;

    var kindName = type.Name;
    if (!kindName.EndsWith("Kind")) continue;
    var alName = kindName[..^"Kind".Length];

    if (!runtimeEnumNames.Contains(alName)) continue;

    var fields = type.GetFields(BindingFlags.Public | BindingFlags.Static);
    var values = fields.Select(f => f.Name).ToList();

    Console.Error.WriteLine($"  {alName}: [{string.Join(", ", values)}]");
    runtimeEnumResults.Add(new { name = alName, values });
}

var missingRuntimeEnums = runtimeEnumNames
    .Where(expected => !runtimeEnumResults.Any(result => ((dynamic)result).name == expected))
    .OrderBy(name => name)
    .ToList();
if (missingRuntimeEnums.Count > 0)
{
    Console.Error.WriteLine($"Required runtime enums not found: {string.Join(", ", missingRuntimeEnums)}");
    return 1;
}

// Global AL functions are defined as *StaticBuiltInMethodTypeSymbol nested classes
// inside *ClassTypeSymbol parent classes.
// Parameter info is NOT available through MetadataLoadContext (requires runtime instantiation).

Console.Error.WriteLine("\n--- Extracting built-in function names ---");

static string? ClassToCategory(string? parentName) => parentName switch
{
    string name when name.StartsWith("Dialog") => "dialog",
    string name when name.StartsWith("Text") => "string",
    string name when name.StartsWith("System") => "system",
    string name when name.StartsWith("File") => "file",
    string name when name.StartsWith("Page") => "system",
    string name when name.StartsWith("EnumType") => "type",
    string name when name.StartsWith("Report") => "system",
    _ => null
};

var discoveredFunctions = new List<(string Name, string Category)>();

foreach (var type in allTypes)
{
    if (!type.Name.EndsWith("StaticBuiltInMethodTypeSymbol")) continue;
    if (type.Name.Contains("<")) continue;

    var methodName = type.Name[..^"StaticBuiltInMethodTypeSymbol".Length];
    var category = ClassToCategory(type.DeclaringType?.Name);
    if (category == null)
    {
        Console.Error.WriteLine(
            $"No category mapping for built-in parent type: {type.DeclaringType?.Name ?? "<none>"}");
        return 1;
    }

    discoveredFunctions.Add((methodName, category));
    Console.Error.WriteLine($"  {category}/{methodName}");
}

Console.Error.WriteLine($"  Discovered {discoveredFunctions.Count} static built-in functions in DLL");

var existingFunctions = new List<JsonElement>();
var existingFunctionsPath = Path.Combine(outputDir, "builtin_functions.json");
if (File.Exists(existingFunctionsPath))
{
    var json = File.ReadAllText(existingFunctionsPath);
    using var doc = JsonDocument.Parse(json);
    foreach (var el in doc.RootElement.EnumerateArray())
        existingFunctions.Add(el.Clone());
    Console.Error.WriteLine($"  Loaded {existingFunctions.Count} entries from existing JSON for enrichment");
}

foreach (var entry in existingFunctions)
{
    if (!entry.TryGetProperty("name", out var name) || string.IsNullOrWhiteSpace(name.GetString()))
    {
        Console.Error.WriteLine("Existing built-in metadata contains an entry without a name");
        return 1;
    }
}

var existingByName = existingFunctions
    .ToDictionary(
        e => e.GetProperty("name").GetString()!,
        e => e,
        StringComparer.OrdinalIgnoreCase);

var builtinOutput = new List<JsonElement>();
var emittedNames = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
var missingMetadata = new List<string>();

foreach (var (name, category) in discoveredFunctions.OrderBy(f => f.Name))
{
    if (emittedNames.Contains(name)) continue;
    emittedNames.Add(name);

    if (existingByName.TryGetValue(name, out var existing))
    {
        var entry = RebuildWithCategory(existing, category);
        builtinOutput.Add(entry);
    }
    else
    {
        missingMetadata.Add(name);
    }
}

if (missingMetadata.Count > 0)
{
    Console.Error.WriteLine(
        $"Missing metadata for discovered built-ins: {string.Join(", ", missingMetadata)}");
    return 1;
}

// MetadataLoadContext does not expose every built-in implementation, so retain
// existing documented entries that were not visible through reflection.
foreach (var existing in existingFunctions)
{
    var name = existing.GetProperty("name").GetString()!;
    if (emittedNames.Contains(name)) continue;
    emittedNames.Add(name);
    builtinOutput.Add(existing);
}

Console.Error.WriteLine($"  Total built-in functions in output: {builtinOutput.Count}");

Console.Error.WriteLine("\n--- Implicit variables (static, not in DLL) ---");
Console.Error.WriteLine("  Keeping existing implicit_variables.json (not extractable from DLL)");

var jsonOptions = new JsonSerializerOptions
{
    WriteIndented = true,
    DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
};

var enumsPath = Path.Combine(outputDir, "runtime_enums.json");
var enumsJson = JsonSerializer.Serialize(runtimeEnumResults, jsonOptions);
await File.WriteAllTextAsync(enumsPath, enumsJson + "\n");
Console.Error.WriteLine($"\nWrote {enumsPath}");

var builtinsPath = Path.Combine(outputDir, "builtin_functions.json");
await WriteBuiltinFunctionsAsync(builtinsPath, builtinOutput);
Console.Error.WriteLine($"Wrote {builtinsPath}");

Console.Error.WriteLine("\nExtraction complete.");
return 0;

static JsonElement RebuildWithCategory(JsonElement existing, string newCategory)
{
    using var ms = new System.IO.MemoryStream();
    using var writer = new Utf8JsonWriter(ms);
    writer.WriteStartObject();
    foreach (var prop in existing.EnumerateObject())
    {
        if (prop.Name != "category")
        {
            prop.WriteTo(writer);
        }
    }
    writer.WriteString("category", newCategory);
    writer.WriteEndObject();
    writer.Flush();
    return JsonDocument.Parse(ms.ToArray()).RootElement.Clone();
}

static async Task WriteBuiltinFunctionsAsync(string path, List<JsonElement> items)
{
    using var ms = new System.IO.MemoryStream();
    var opts = new JsonWriterOptions { Indented = true };
    using var writer = new Utf8JsonWriter(ms, opts);

    writer.WriteStartArray();
    foreach (var item in items)
    {
        item.WriteTo(writer);
    }
    writer.WriteEndArray();
    await writer.FlushAsync();

    var bytes = ms.ToArray();
    using var fs = File.Create(path);
    await fs.WriteAsync(bytes);
    await fs.WriteAsync("\n"u8.ToArray());
}

static Version? ExtensionVersion(string directory)
{
    const string prefix = "ms-dynamics-smb.al-";
    var name = Path.GetFileName(directory);
    if (!name.StartsWith(prefix, StringComparison.OrdinalIgnoreCase)) return null;
    return Version.TryParse(name[prefix.Length..], out var version) ? version : null;
}
