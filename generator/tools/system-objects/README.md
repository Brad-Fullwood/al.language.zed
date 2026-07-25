# system-objects

Extracts the system-permission object names and IDs from
`Microsoft.Dynamics.Nav.CodeAnalysis.dll`. The IDs come from `SystemObjects`
constants and the display names from the embedded `SystemObjectsResources`
table.

```sh
TCDIR=/path/to/al/toolchain dotnet run -c Release \
  > ../../../data/system_objects.json
```

Regenerate the committed data after upgrading the AL toolchain.
