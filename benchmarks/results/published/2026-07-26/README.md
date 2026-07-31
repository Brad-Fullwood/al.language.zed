# Raw benchmark evidence — 2026-07-26

These files are exact copies of the publishable scratch outputs described in
[`../../../FINDINGS.md`](../../../FINDINGS.md).

Accuracy, emitter, and symbol results were generated from clean commit
`d21d0475651bff5597d14ec5974e2f9610ab76aa`. LSP results were generated from
clean commit `50d3bcc85614ee8fdd87ca2171e834181ad89d3b`, after the harness was
changed to include successful semantic analysis in cold-ready time and to
require that evidence before probes.

| File | SHA-256 |
|---|---|
| `accuracy.json` | `a040d718e57b839d6f0584cd192cef6445995eec1c1d2415097010a8dd865123` |
| `accuracy_al_lsp.stderr.log` | `56cd7db46ddc57cfb07ebd68a78e74dcb609df3c1dd03c059de11601c67074f7` |
| `emit.json` | `882507ad2064570d54d0a36f6629e7be7566a6d360a70c3c88e8e4d458f9df99` |
| `lsp_al_medium.json` | `ec4554cfcd3f36d0edd9996834fc9dc2458157070955330884cd3de9114a677f` |
| `lsp_al_medium.stderr.log` | `a5e1b0a80ca8eb2f57091c5351028b0bf214ed912909601f0ff98251c9b57052` |
| `lsp_ms_medium.json` | `998543b4308379bc5bd36092c1a98d31d60809e06eccab43b4482c49478597f5` |
| `lsp_ms_medium.stderr.log` | `e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855` |
| `symbols.json` | `3c319fcd19165423ae75773954a840f061f4b864551c57dff699854760d0414a` |

The empty Microsoft stderr file is intentional and its empty-file digest is
recorded. Each JSON file also carries the repository state, machine, binary
hashes, package hashes, methodology, individual samples, and validity result.
