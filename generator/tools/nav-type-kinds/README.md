# nav-type-kinds

Extracts the `Microsoft.Dynamics.Nav.CodeAnalysis.NavTypeKind` names and IDs used
by native `.app` method-ID hashing.

```sh
TCDIR=/path/to/al/toolchain dotnet run -c Release \
  > ../../../data/nav_type_kinds.json
```

Regenerate the committed data after upgrading the AL toolchain.
