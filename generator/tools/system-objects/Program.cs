// Extracts the AL system-object permission table from CodeAnalysis metadata.
// Usage: TCDIR=<toolchain dir> dotnet run -c Release > system_objects.json

using System.Collections;
using System.Reflection.Metadata;
using System.Reflection.PortableExecutable;
using System.Resources;
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

var systemObjectTypes = metadata.TypeDefinitions
    .Where(handle => metadata.GetString(metadata.GetTypeDefinition(handle).Name) == "SystemObjects")
    .ToList();
if (systemObjectTypes.Count != 1)
    throw new InvalidDataException($"Expected one SystemObjects type, found {systemObjectTypes.Count}");

var constants = new Dictionary<string, int>(StringComparer.Ordinal);
foreach (var fieldHandle in metadata.GetTypeDefinition(systemObjectTypes[0]).GetFields())
{
    var field = metadata.GetFieldDefinition(fieldHandle);
    var constantHandle = field.GetDefaultValue();
    if (constantHandle.IsNil) continue;

    var constant = metadata.GetConstant(constantHandle);
    if (constant.TypeCode != ConstantTypeCode.Int32) continue;
    var name = metadata.GetString(field.Name);
    constants.Add(name, metadata.GetBlobReader(constant.Value).ReadInt32());
}
if (constants.Count == 0)
    throw new InvalidDataException("SystemObjects contains no integer constants");

var matchingResources = metadata.ManifestResources
    .Where(handle => metadata.GetString(metadata.GetManifestResource(handle).Name)
        .Contains("SystemObjectsResources", StringComparison.Ordinal))
    .ToList();
if (matchingResources.Count != 1)
    throw new InvalidDataException(
        $"Expected one SystemObjectsResources resource, found {matchingResources.Count}");

var manifestResource = metadata.GetManifestResource(matchingResources[0]);
if (!manifestResource.Implementation.IsNil)
    throw new InvalidDataException("SystemObjectsResources is stored in an external assembly");

var corHeader = peReader.PEHeaders.CorHeader
    ?? throw new BadImageFormatException("Assembly has no CLR header", assemblyPath);
var resourceBlock = peReader.GetSectionData(
    checked(corHeader.ResourcesDirectory.RelativeVirtualAddress + (int)manifestResource.Offset));
var resourceBlob = resourceBlock.GetReader();
var resourceLength = resourceBlob.ReadInt32();
if (resourceLength < 0 || resourceLength > resourceBlob.RemainingBytes)
    throw new InvalidDataException("SystemObjectsResources has an invalid length");

var captions = new Dictionary<string, string>(StringComparer.Ordinal);
using (var stream = new MemoryStream(resourceBlob.ReadBytes(resourceLength), writable: false))
using (var reader = new ResourceReader(stream))
{
    foreach (DictionaryEntry entry in reader)
    {
        if (entry.Key is string key && entry.Value is string value)
            captions.Add(key, value);
    }
}

var result = new SortedDictionary<string, int>(StringComparer.Ordinal);
foreach (var (constantName, id) in constants)
{
    if (captions.TryGetValue(constantName + "SystemObjectCaption", out var displayName))
        result.Add(displayName, id);
}
if (result.Count == 0)
    throw new InvalidDataException("No system-object constants matched resource captions");

Console.WriteLine(JsonSerializer.Serialize(result, new JsonSerializerOptions { WriteIndented = true }));
