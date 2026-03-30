# al-symbols — Package Symbol Index (~6K lines)

**Leaf crate** — must NOT depend on al-syntax, al-semantic, or al-core.

## Quick Reference

```sh
cargo test -p al-symbols                    # all tests (~68 inline + 2 integration)
cargo test -p al-symbols --test corpus      # .app file corpus tests
cargo test -p al-symbols --test perf_audit  # performance benchmarks
```

## Key Exports (lib.rs)

- `SymbolIndex` — DashMap-backed symbol lookup by name/kind
- `read_app_file`, `read_app_bytes` — .app file parsing
- `NuGetClient`, `NuGetFeed`, `PackageRef`, `AppDependency` — NuGet package management
- `NavxManifest`, `parse_manifest` — app manifest parsing
- `get_events`, `EventPublisher`, `EventSubscriber`, `EventType` — event discovery
- `get_composed` — cross-package symbol composition
- Model: `AlObject`, `AlMethod`, `AlField`, `AlProperty`, `AlParameter`, `AlEnumType`, `AlEnumValue`

## .app File Format (CRITICAL GOTCHAS)

- 40-byte NAVX header, then ZIP archive
- `SymbolReference.json` inside ZIP has **UTF-8 BOM** (3 bytes: 0xEF 0xBB 0xBF) — strip before JSON parse
- JSON key is `EnumTypes` NOT `Enums`
- `Kind` field is integer in newer BC versions, not string

## NuGet

- Hostname: `dynamicssmb2.pkgs.visualstudio.com` (NOT `dynamicssmb` — common typo)
- Cached at `~/.cache/al-lsp/packages/`
- OAuth flow in `oauth.rs` for authenticated feeds

## Modules

| File | Purpose |
|------|---------|
| app_reader.rs | .app parsing (NAVX header + ZIP extraction) |
| index.rs | SymbolIndex (DashMap-backed) |
| model.rs | AL object model types (AlObject, AlMethod, etc.) |
| nuget.rs | NuGet client, feed handling, package download |
| oauth.rs | OAuth2 authentication for NuGet feeds |
| manifest.rs | app.json manifest parsing (NavxManifest) |
| events.rs | event publisher/subscriber discovery |
| composition.rs | cross-package symbol composition |
| source_index.rs | workspace .al file symbol extraction |
| cache.rs | symbol cache management |
| virtual_file.rs | virtual file generation for go-to-definition on packages |
| bc_server.rs | Business Central server package discovery |
| language_data.rs | supplementary language data from packages |
