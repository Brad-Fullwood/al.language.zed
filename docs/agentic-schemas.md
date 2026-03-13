# Agentic Output Schemas

## Design Principles

1. **Token Density**: Every JSON field earns its place. No verbose keys, no null fields, no wrapper objects.
2. **250-Token Budget**: A complex cross-file event trace must fit in ~250 tokens.
3. **Actionable References**: Every result includes file path + line + column so the agent can navigate directly.
4. **Progressive Disclosure**: Summary first, details on demand. Agents call `search` then `object` then `hover`.
5. **1:1 CLI/MCP Parity**: Same schema from `al search --json` and MCP `al/search` tool.

## Slash Command Output Formats

Zed slash commands inject context into the AI assistant. These must be maximally dense.

### `/al-symbols <query>`
Searches workspace and package symbols. Output:
```json
{"symbols":[
  {"k":"Table","id":18,"n":"Customer","pkg":"Base Application","f":"src/Customer.al","l":1},
  {"k":"Page","id":21,"n":"Customer Card","pkg":"Base Application","f":null,"l":null}
]}
```
Fields: `k`=kind, `id`=object ID, `n`=name, `pkg`=package, `f`=file (null=package-only), `l`=line.

### `/al-events <name>`
Traces event publisher/subscriber chains:
```json
{"event":"OnAfterPostSalesDocument","pub":{"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostSalesDocument","l":245},
"subs":[
  {"k":"Codeunit","id":50100,"n":"My Subscriber","proc":"OnAfterPostSalesDocument","f":"src/MySub.al","l":32,"pkg":"workspace"},
  {"k":"Codeunit","id":1535,"n":"Approvals Mgmt","proc":"OnAfterPostSalesDoc","f":null,"l":null,"pkg":"Base Application"}
]}
```
Budget: ~150 tokens for publisher + 5 subscribers.

### `/al-object <name>`
Full object API surface:
```json
{"k":"Table","id":18,"n":"Customer","fields":[
  {"id":1,"n":"No.","t":"Code[20]","pk":true},
  {"id":2,"n":"Name","t":"Text[100]"}
],"procs":[
  {"n":"SetFilter","params":"FilterStr: Text","ret":""},
  {"n":"GetBalance","params":"","ret":"Decimal"}
],"events":["OnAfterModifyEvent","OnBeforeDeleteEvent"]}
```

### `/al-trace <file> <line>`
Call chain trace from a position:
```json
{"origin":{"f":"src/SalesPost.al","l":120,"proc":"PostDocument"},
"chain":[
  {"depth":1,"k":"call","target":"ValidateCustomer","obj":"Sales-Post","l":125},
  {"depth":2,"k":"call","target":"CheckCreditLimit","obj":"Sales-Post","l":340},
  {"depth":2,"k":"event","target":"OnCheckCreditLimit","subs":3},
  {"depth":3,"k":"call","target":"LogEntry","obj":"My Logger","l":15}
]}
```

### `/al-deps`
Dependency graph summary:
```json
{"app":"My Extension","version":"1.0.0",
"deps":[
  {"n":"Base Application","v":"26.0.0.0","id":"437dbf0e-84ff-417a-965d-ed2bb9650972"},
  {"n":"System Application","v":"26.0.0.0","id":"63ca2fa4-4f03-4f2b-a480-172fef340d3f"}
],
"depBy":[]}
```

### `/al-source <object> [procedure|trigger]`
Targeted source code extraction. Avoids reading entire files — returns just the requested scope.

**Workspace object (full):**
```json
{"k":"Codeunit","id":50100,"n":"Sales Processor","src":"workspace",
"code":"codeunit 50100 \"Sales Processor\"\n{\n    procedure PostDocument(var SalesHeader: Record \"Sales Header\")\n    var\n        ...\n    begin\n        ...\n    end;\n}"}
```

**Workspace object (specific procedure):**
```json
{"k":"Codeunit","id":50100,"n":"Sales Processor","proc":"PostDocument","src":"workspace",
"range":{"f":"src/SalesProcessor.al","l":25,"end":85},
"sig":"procedure PostDocument(var SalesHeader: Record \"Sales Header\")",
"code":"    procedure PostDocument(var SalesHeader: Record \"Sales Header\")\n    var\n        Customer: Record Customer;\n    begin\n        ValidateHeader(SalesHeader);\n        ...\n    end;"}
```

**Package object (source available in .app):**
```json
{"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostSalesDocument","src":"package",
"pkg":"Base Application",
"sig":"procedure PostSalesDocument(var SalesHeader: Record \"Sales Header\")",
"code":"    procedure PostSalesDocument(var SalesHeader: Record \"Sales Header\")\n    begin\n        ...\n    end;"}
```

**Package object (no source in .app — rendered outline from SymbolReference.json):**
```json
{"k":"Table","id":18,"n":"Customer","src":"outline","pkg":"Base Application",
"code":"table 18 Customer\n{\n    fields\n    {\n        field(1; \"No.\"; Code[20]) { }\n        field(2; Name; Text[100]) { }\n    }\n\n    procedure SetFilter(FilterStr: Text)\n    procedure GetBalance(): Decimal\n}",
"note":"Rendered from symbol metadata — full signatures and fields, no implementation bodies"}
```

Budget: ~80 tokens for a single procedure, ~200-400 for a full object depending on size.

**No fallbacks.** If source is in the .app, extract it. If not, render a complete outline from SymbolReference.json with full procedure signatures (params + return types), field definitions (id + type), keys, enum values, and event declarations. The outline must include everything available in the symbol metadata — it is the primary output, not a degraded mode.

### `/al-tables <object> [procedure]`
What tables does this object/procedure read/write? Traces through Record variable types and their method calls.
```json
{"object":"Sales-Post","id":80,"tables":[
  {"id":36,"n":"Sales Header","ops":["get","modify","delete"],"procs":["PostDocument","FinalizePost"]},
  {"id":37,"n":"Sales Line","ops":["findset","modify","insert"],"procs":["PostLines"]},
  {"id":21,"n":"Cust. Ledger Entry","ops":["insert"],"procs":["PostCustLedger"]}
]}
```
`ops` = Record operations found: get, find, findset, findfirst, findlast, insert, modify, delete, modifyall, deleteall, setrange, setfilter, calcsums, calcfields, reset, init.

### `/al-callgraph <object> [procedure] [--depth N]`
Transitive call chain across object boundaries. Includes procedure calls, event emissions, and table touches.
```json
{"root":{"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostDocument"},
"depth":3,"calls":[
  {"d":1,"type":"call","proc":"ValidateHeader","obj":"Sales-Post","tables":["Sales Header"]},
  {"d":1,"type":"call","proc":"PostLines","obj":"Sales-Post","tables":["Sales Line","Item Ledger Entry"]},
  {"d":2,"type":"call","proc":"CheckCreditLimit","obj":"Customer Mgt.","tables":["Customer"]},
  {"d":1,"type":"event","name":"OnAfterPostDocument","subs":3},
  {"d":2,"type":"sub","proc":"HandlePostDoc","obj":"My Extension","tables":["Custom Log"]}
]}
```
Default depth: 3. Max depth: 10.

### `/al-intercept <object> [--field <name>] [--proc <name>]`
Find events where an extension can intercept and alter behavior. Returns integration/business events with `var` parameters (modifiable by subscriber).
```json
{"object":"Sales-Post","events":[
  {"name":"OnBeforePostSalesDoc","type":"integration","proc":"PostDocument","l":120,
   "params":["var SalesHeader: Record \"Sales Header\"","var IsHandled: Boolean"],
   "note":"var params — subscriber can modify SalesHeader or set IsHandled to skip default logic"},
  {"name":"OnAfterCheckCreditLimit","type":"business","proc":"CheckCreditLimit","l":340,
   "params":["Customer: Record Customer","var CreditOK: Boolean"],
   "note":"var CreditOK — subscriber can override credit check result"}
]}
```
With `--field`: filters to events whose parameters include that field's table. With `--proc`: filters to events published within that procedure.

### `/al-subscribers <object>`
All subscribers to events published by this object, grouped by event.
```json
{"publisher":{"k":"Codeunit","id":80,"n":"Sales-Post"},"events":[
  {"name":"OnAfterPostDocument","type":"integration","subs":[
    {"k":"Codeunit","id":50100,"n":"My Extension","proc":"OnAfterPost","f":"src/MyExt.al","l":32,"pkg":"workspace"},
    {"k":"Codeunit","id":1535,"n":"Approvals Mgmt","proc":"OnAfterPostSalesDoc","pkg":"Base Application"}
  ]},
  {"name":"OnBeforePostLines","type":"integration","subs":[]}
]}
```

### `/al-debug <subcommand>`
Agentic debugger control. The daemon manages a headless debug session (compile → EditorServices.Host → DAP).

**Start a debug session:**
```json
{"cmd":"start","config":"default","status":"compiling"}
```
Then async:
```json
{"cmd":"start","status":"running","session":"s1","pid":12345}
```

**Set breakpoints:**
```json
{"cmd":"breakpoints","set":[
  {"f":"src/SalesProcessor.al","l":32,"id":"bp1"},
  {"f":"src/SalesProcessor.al","l":55,"id":"bp2","condition":"SalesHeader.\"No.\" = 'S-1001'"}
]}
```

**When paused at breakpoint — get current state:**
```json
{"cmd":"state","session":"s1","status":"paused",
"location":{"f":"src/SalesProcessor.al","l":32,"proc":"PostDocument"},
"stack":[
  {"frame":0,"proc":"PostDocument","obj":"Sales Processor","f":"src/SalesProcessor.al","l":32},
  {"frame":1,"proc":"OnRun","obj":"Sales Processor","f":"src/SalesProcessor.al","l":5}
],
"vars":[
  {"name":"SalesHeader","type":"Record \"Sales Header\"","fields":[
    {"n":"No.","v":"S-1001"},{"n":"Posting Date","v":"2026-03-13"},{"n":"Status","v":"Open"}
  ]},
  {"name":"IsHandled","type":"Boolean","v":"false"},
  {"name":"LineCount","type":"Integer","v":"3"}
]}
```

**Evaluate expression:**
```json
{"cmd":"eval","expr":"SalesHeader.\"Sell-to Customer No.\"","result":{"type":"Code[20]","v":"C-10000"}}
```

**Step/continue:**
```json
{"cmd":"continue","status":"paused","location":{"f":"src/SalesProcessor.al","l":55,"proc":"PostDocument"}}
```
```json
{"cmd":"step","type":"over","status":"paused","location":{"f":"src/SalesProcessor.al","l":33,"proc":"PostDocument"}}
```

**Debug history (recorded snapshots at each breakpoint hit):**
```json
{"cmd":"history","session":"s1","hits":[
  {"seq":1,"bp":"bp1","time":"14:32:01.123",
   "location":{"f":"src/SalesProcessor.al","l":32,"proc":"PostDocument"},
   "vars":[{"name":"SalesHeader.\"No.\"","v":"S-1001"},{"name":"IsHandled","v":"false"}]},
  {"seq":2,"bp":"bp2","time":"14:32:01.456",
   "location":{"f":"src/SalesProcessor.al","l":55,"proc":"PostDocument"},
   "vars":[{"name":"SalesHeader.\"No.\"","v":"S-1001"},{"name":"LineCount","v":"3"}]},
  {"seq":3,"bp":"bp1","time":"14:32:02.789",
   "location":{"f":"src/SalesProcessor.al","l":32,"proc":"PostDocument"},
   "vars":[{"name":"SalesHeader.\"No.\"","v":"S-1002"},{"name":"IsHandled","v":"false"}]}
]}
```

**Stop session:**
```json
{"cmd":"stop","session":"s1","status":"stopped","hits":3}
```

Budget: ~100 tokens for state, ~150 for history with 3 hits. The agent sees exactly what a human would see in the debugger UI.

### `/al-lint`
Current file diagnostics (injected as AI context):
```json
{"f":"src/MySub.al","diags":[
  {"l":32,"c":5,"sev":"warn","code":"AL0604","msg":"Use of implicit 'with' is deprecated"},
  {"l":45,"c":1,"sev":"error","code":"AA0005","msg":"Missing documentation comment"}
]}
```

## CLI JSON Schemas (--json output)

### `al search --json <query>`
```json
[{"k":"Table","id":18,"n":"Customer","pkg":"Base Application","f":null}]
```

### `al object --json <name>`
Same as `/al-object` slash command schema.

### `al events --json <name>`
Same as `/al-events` slash command schema.

### `al subscribers --json <event>`
```json
[{"k":"Codeunit","id":50100,"n":"My Sub","proc":"HandleEvent","f":"src/MySub.al","l":32}]
```

### `al source --json <object> [--proc <name>] [--trigger <name>]`
Targeted source extraction. Three resolution levels:
- No filter: full object source
- `--proc <name>`: just that procedure's source + signature
- `--trigger <name>`: just that trigger's source

```json
{"k":"Codeunit","id":50100,"n":"Sales Processor","proc":"PostDocument","src":"workspace",
"range":{"f":"src/SalesProcessor.al","l":25,"end":85},
"sig":"procedure PostDocument(var SalesHeader: Record \"Sales Header\")",
"code":"    procedure PostDocument(...)\n    begin\n        ...\n    end;"}
```

`src` field: `"workspace"` (full source from .al file), `"package"` (source extracted from .app), or `"outline"` (rendered from SymbolReference.json — full signatures and fields, no implementation bodies).

### `al hover --json <file> <line> <col>`
```json
{"symbol":{"k":"Table","id":18,"n":"Customer"},"doc":"Table 18 Customer\nFields: 150\nSource: Base Application v26.0","sig":"Record \"Customer\""}
```

### `al definition --json <file> <line> <col>`
```json
{"target":{"f":"src/Customer.al","l":1,"c":1},"symbol":{"k":"Table","id":18,"n":"Customer"}}
```
If target is in a package (no source file): `{"target":null,"symbol":{...},"pkg":"Base Application"}`

### `al references --json <file> <line> <col>`
```json
{"symbol":"Customer","refs":[
  {"f":"src/SalesHeader.al","l":45,"c":12,"ctx":"var Cust: Record Customer;"},
  {"f":"src/PostSales.al","l":120,"c":8,"ctx":"Customer.Get(CustNo);"}
]}
```
`ctx` = trimmed line content for quick understanding.

### `al completions --json <file> <line> <col>`
```json
[{"label":"Customer","k":"Table","detail":"Table 18","insert":"Customer"},
 {"label":"CustomerCard","k":"Page","detail":"Page 21","insert":"CustomerCard"}]
```

### `al lint --json [--all]`
```json
{"files":[
  {"f":"src/MySub.al","diags":[{"l":32,"c":5,"sev":"warn","code":"AL0604","msg":"..."}]}
]}
```

### `al fix --json [--dry-run]`
```json
{"fixes":[
  {"f":"src/MySub.al","l":32,"code":"AL0604","action":"replace","old":"with Customer do","new":"Customer.\"No.\""}
]}
```

### `al debug --json <subcommand>`
Headless debugger control. Subcommands:

```bash
al debug start [--config <name>]          # compile + launch debug session
al debug breakpoint <file> <line> [--condition <expr>]  # set breakpoint
al debug breakpoint --clear [--all]       # remove breakpoints
al debug state                            # current location, stack, variables (when paused)
al debug eval <expr>                      # evaluate expression at current frame
al debug continue                         # resume execution
al debug step [over|into|out]             # step execution
al debug history [--var <name>]           # recorded snapshots at breakpoint hits
al debug stop                             # end session
```

All responses use `/al-debug` slash command schema. `--var` filter on history returns only snapshots where that variable changed.

### `al deps --json`
```json
{"app":"My Extension","deps":[{"n":"Base Application","v":"26.0.0.0","direct":true}],"transitive":[{"n":"System","v":"26.0.0.0"}]}
```

### `al metrics --json [file]`
```json
{"files":[
  {"f":"src/SalesPost.al","procs":[
    {"n":"PostDocument","lines":85,"cyclomatic":12,"cognitive":18,"calls":7},
    {"n":"ValidateHeader","lines":20,"cyclomatic":3,"cognitive":4,"calls":2}
  ]}
]}
```

### `al impact --json <symbol>`
```json
{"symbol":"Customer.\"Credit Limit\"","impacted":[
  {"k":"Page","id":21,"n":"Customer Card","field":"Credit Limit (LCY)","type":"display"},
  {"k":"Codeunit","id":80,"n":"Sales-Post","proc":"CheckCreditLimit","l":340,"type":"read"},
  {"k":"Report","id":111,"n":"Customer - Top 10","dataitem":"Customer","type":"filter"}
]}
```

### `al dead-code --json`
```json
{"unused":[
  {"k":"procedure","n":"OldHelper","obj":"My Codeunit","f":"src/MyCU.al","l":120,"reason":"zero references"},
  {"k":"field","id":50100,"n":"Legacy Flag","obj":"My Table","f":"src/MyTable.al","l":45,"reason":"zero references"},
  {"k":"subscriber","n":"OnOldEvent","obj":"My Sub","f":"src/MySub.al","l":80,"reason":"publisher removed in BC26"}
]}
```

### `al tables --json <object> [--proc <name>]`
Same as `/al-tables` slash command schema.

### `al callgraph --json <object> [--proc <name>] [--depth N]`
Same as `/al-callgraph` slash command schema.

### `al intercept --json <object> [--field <name>] [--proc <name>]`
Same as `/al-intercept` slash command schema.

### `al subscribers --json <object>`
Same as `/al-subscribers` slash command schema (object-level, grouped by event).

---

## MCP Tool Schemas

MCP tools use the same response schemas as CLI `--json` output. Tool registration:

```json
{"tools":[
  {"name":"al/search","description":"Search AL symbols","inputSchema":{"type":"object","properties":{"query":{"type":"string"},"kind":{"type":"string","enum":["Table","Page","Codeunit","Report","Query","Enum","Interface"]}}}},
  {"name":"al/object","description":"Get AL object details","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"kind":{"type":"string"}},"required":["name"]}},
  {"name":"al/source","description":"Get AL source code for an object, procedure, or trigger","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"kind":{"type":"string"},"proc":{"type":"string","description":"Procedure name filter"},"trigger":{"type":"string","description":"Trigger name filter"}},"required":["name"]}},
  {"name":"al/events","description":"Trace event chains","inputSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}},
  {"name":"al/hover","description":"Hover info at position","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"line":{"type":"integer"},"col":{"type":"integer"}},"required":["file","line","col"]}},
  {"name":"al/definition","description":"Go to definition","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"line":{"type":"integer"},"col":{"type":"integer"}},"required":["file","line","col"]}},
  {"name":"al/references","description":"Find all references","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"line":{"type":"integer"},"col":{"type":"integer"}},"required":["file","line","col"]}},
  {"name":"al/lint","description":"Lint files","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"all":{"type":"boolean"}}}},
  {"name":"al/trace","description":"Trace call chain","inputSchema":{"type":"object","properties":{"file":{"type":"string"},"line":{"type":"integer"}},"required":["file","line"]}},
  {"name":"al/impact","description":"Dependency impact analysis","inputSchema":{"type":"object","properties":{"symbol":{"type":"string"}},"required":["symbol"]}},
  {"name":"al/metrics","description":"Code complexity metrics","inputSchema":{"type":"object","properties":{"file":{"type":"string"}}}},
  {"name":"al/tables","description":"What tables does this object/procedure read/write?","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"proc":{"type":"string"}},"required":["name"]}},
  {"name":"al/callgraph","description":"Transitive call graph across object boundaries","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"proc":{"type":"string"},"depth":{"type":"integer","default":3,"maximum":10}},"required":["name"]}},
  {"name":"al/intercept","description":"Find events where behavior can be intercepted/altered","inputSchema":{"type":"object","properties":{"name":{"type":"string"},"field":{"type":"string"},"proc":{"type":"string"}},"required":["name"]}},
  {"name":"al/subscribers","description":"All subscribers to events published by this object","inputSchema":{"type":"object","properties":{"name":{"type":"string"}},"required":["name"]}},
  {"name":"al/debug","description":"Headless debugger control","inputSchema":{"type":"object","properties":{"cmd":{"type":"string","enum":["start","breakpoint","state","eval","continue","step","history","stop"]},"file":{"type":"string"},"line":{"type":"integer"},"condition":{"type":"string"},"expr":{"type":"string"},"step_type":{"type":"string","enum":["over","into","out"]},"var":{"type":"string","description":"Filter history to snapshots where this variable changed"},"config":{"type":"string","description":"Launch config name"}},"required":["cmd"]}},
  {"name":"al/dead-code","description":"Find unused code","inputSchema":{"type":"object","properties":{}}},
  {"name":"al/permissions","description":"Generate permission set","inputSchema":{"type":"object","properties":{"format":{"type":"string","enum":["al","xml"]}}}}
]}
```

## Token Budget Verification

Example: "What happens when a Sales Order is posted?"

### Without Zed AL (raw file reads):
Agent reads 5 files (SalesPost.al, SalesHeader.al, ApprovalsMgmt.al, etc.) = ~5000-8000 tokens

### With Zed AL:
1. `/al-events OnAfterPostSalesDocument` -> ~150 tokens (publisher + 5 subscribers)
2. `/al-trace src/SalesPost.al 120` -> ~100 tokens (call chain)
3. Total: ~250 tokens = **20x+ density improvement**

## Schema Versioning

All JSON outputs include no version field -- schemas are versioned implicitly by the CLI/MCP version. Breaking changes require a major version bump in `al-cli`.
