// Extracts the NavTypeKind enum from CodeAnalysis metadata.
// Usage: TCDIR=<toolchain dir> dotnet run -c Release > nav_type_kinds.json

using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Text.Json;

var toolchainDir = Environment.GetEnvironmentVariable("TCDIR")
    ?? throw new InvalidOperationException(
        "Set TCDIR to the AL toolchain directory containing Microsoft.Dynamics.Nav.CodeAnalysis.dll");
var assemblyPath = Path.Combine(toolchainDir, "Microsoft.Dynamics.Nav.CodeAnalysis.dll");
if (!File.Exists(assemblyPath))
    throw new FileNotFoundException("CodeAnalysis assembly not found", assemblyPath);

using var assemblyStream = File.OpenRead(assemblyPath);
using var peReader = new PEReader(assemblyStream);
var metadata = peReader.GetMetadataReader();

var matchingTypes = metadata.TypeDefinitions
    .Where(handle => metadata.GetString(metadata.GetTypeDefinition(handle).Name) == "NavTypeKind")
    .ToList();
if (matchingTypes.Count != 1)
    throw new InvalidDataException($"Expected one NavTypeKind type, found {matchingTypes.Count}");

var result = new SortedDictionary<string, int>(StringComparer.Ordinal);
foreach (var fieldHandle in metadata.GetTypeDefinition(matchingTypes[0]).GetFields())
{
    var field = metadata.GetFieldDefinition(fieldHandle);
    var name = metadata.GetString(field.Name);
    if (name.StartsWith('_')) continue;

    var constantHandle = field.GetDefaultValue();
    if (constantHandle.IsNil) continue;
    var constant = metadata.GetConstant(constantHandle);
    if (constant.TypeCode != ConstantTypeCode.Int32) continue;
    result.Add(name, metadata.GetBlobReader(constant.Value).ReadInt32());
}
if (result.Count == 0)
    throw new InvalidDataException("NavTypeKind contains no integer enum values");

Console.WriteLine(JsonSerializer.Serialize(result, new JsonSerializerOptions { WriteIndented = true }));
