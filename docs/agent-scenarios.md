# Agent Discovery Scenarios

Real-world questions an AI agent building a BC extension would ask, mapped to CLI/MCP queries.
Each scenario is a test case for the insight engine (WP9).

---

## Scenario 1: Table Impact Analysis

**Agent question**: "When Sales-Post codeunit runs, what tables are affected?"

```bash
al tables "Sales-Post" --json
```
```json
{"object":"Sales-Post","id":80,"tables":[
  {"id":36,"n":"Sales Header","ops":["get","modify","delete"],"procs":["PostDocument","FinalizePost"]},
  {"id":37,"n":"Sales Line","ops":["findset","modify","insert"],"procs":["PostLines"]},
  {"id":21,"n":"Cust. Ledger Entry","ops":["insert"],"procs":["PostCustLedger"]},
  {"id":32,"n":"Item Ledger Entry","ops":["insert"],"procs":["PostItemLedger"]}
]}
```

**What the agent learns**: Which tables are read vs written, and which procedures touch them. No file reads needed.

---

## Scenario 2: Deep Call Graph

**Agent question**: "What procedures are called when PostDocument runs, including cross-codeunit calls?"

```bash
al callgraph "Sales-Post" --proc PostDocument --depth 3 --json
```
```json
{"root":{"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostDocument"},
"depth":3,"calls":[
  {"d":1,"type":"call","proc":"ValidateHeader","obj":"Sales-Post","tables":["Sales Header"]},
  {"d":1,"type":"call","proc":"PostLines","obj":"Sales-Post","tables":["Sales Line"]},
  {"d":2,"type":"call","proc":"PostItemJnlLine","obj":"Item Jnl.-Post Line","tables":["Item Ledger Entry"]},
  {"d":2,"type":"call","proc":"CheckCreditLimit","obj":"Cust. Check Cr. Limit","tables":["Customer"]},
  {"d":1,"type":"event","name":"OnAfterPostDocument","subs":3},
  {"d":2,"type":"sub","proc":"HandlePostDoc","obj":"My Extension","tables":["Custom Log"]}
]}
```

**What the agent learns**: Full transitive call chain with table touches at every level. Events show subscriber count so agent knows where extensions hook in.

---

## Scenario 3: Event Interception

**Agent question**: "I need to modify the Sales Header before posting. Where can I intercept this?"

```bash
al intercept "Sales-Post" --field "Sales Header" --json
```
```json
{"object":"Sales-Post","events":[
  {"name":"OnBeforePostSalesDoc","type":"integration","proc":"PostDocument","l":120,
   "params":["var SalesHeader: Record \"Sales Header\"","var IsHandled: Boolean"],
   "note":"var params — subscriber can modify SalesHeader or set IsHandled to skip default logic"},
  {"name":"OnAfterValidateSalesHeader","type":"integration","proc":"ValidateHeader","l":180,
   "params":["var SalesHeader: Record \"Sales Header\""],
   "note":"var SalesHeader — subscriber can alter validated header before processing continues"}
]}
```

**What the agent learns**: Exact events with `var` parameters that allow modification. The agent knows which procedure publishes each event and can read just that procedure's source if needed.

---

## Scenario 4: Subscriber Discovery

**Agent question**: "What extensions subscribe to Sales-Post events? Are there conflicts I need to worry about?"

```bash
al subscribers "Sales-Post" --json
```
```json
{"publisher":{"k":"Codeunit","id":80,"n":"Sales-Post"},"events":[
  {"name":"OnAfterPostDocument","type":"integration","subs":[
    {"k":"Codeunit","id":50100,"n":"My Extension","proc":"OnAfterPost","f":"src/MyExt.al","l":32,"pkg":"workspace"},
    {"k":"Codeunit","id":1535,"n":"Approvals Mgmt","proc":"OnAfterPostSalesDoc","pkg":"Base Application"},
    {"k":"Codeunit","id":7000,"n":"Assembly Management","proc":"PostSalesAssembly","pkg":"Base Application"}
  ]},
  {"name":"OnBeforePostLines","type":"integration","subs":[
    {"k":"Codeunit","id":50100,"n":"My Extension","proc":"OnBeforeLines","f":"src/MyExt.al","l":55,"pkg":"workspace"}
  ]}
]}
```

**What the agent learns**: All subscribers grouped by event, with source location for workspace subscribers. Agent can check for ordering conflicts or duplicate logic.

---

## Scenario 5: Procedure Source Extraction

**Agent question**: "Show me the PostDocument procedure — I need to understand the flow before subscribing."

```bash
al source "Sales-Post" --proc PostDocument --json
```
```json
{"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostDocument","src":"package",
"pkg":"Base Application",
"sig":"procedure PostDocument(var SalesHeader: Record \"Sales Header\")",
"code":"    procedure PostDocument(var SalesHeader: Record \"Sales Header\")\n    begin\n        OnBeforePostSalesDoc(SalesHeader, IsHandled);\n        if IsHandled then exit;\n        ValidateHeader(SalesHeader);\n        PostLines(SalesHeader);\n        FinalizePost(SalesHeader);\n        OnAfterPostDocument(SalesHeader);\n    end;"}
```

**What the agent learns**: Exact implementation flow — where events fire relative to logic, what's called in what order. One targeted query instead of extracting and reading the entire 2000-line codeunit.

---

## Scenario 6: Field Impact Across Objects

**Agent question**: "If I change Customer.Credit Limit, what breaks?"

```bash
al impact "Customer.\"Credit Limit\"" --json
```
```json
{"symbol":"Customer.\"Credit Limit\"","impacted":[
  {"k":"Page","id":21,"n":"Customer Card","field":"Credit Limit (LCY)","type":"display"},
  {"k":"Codeunit","id":312,"n":"Cust. Check Cr. Limit","proc":"CheckCreditLimit","l":40,"type":"read"},
  {"k":"Codeunit","id":80,"n":"Sales-Post","proc":"PostDocument","l":250,"type":"indirect"},
  {"k":"Report","id":111,"n":"Customer - Top 10","dataitem":"Customer","type":"filter"}
]}
```

**What the agent learns**: Every object that reads, displays, or filters by this field. `indirect` means it's accessed through a call chain, not directly.

---

## Scenario 7: Combined Discovery Workflow

**Agent task**: "Add a custom validation before sales posting that checks a new field on Customer."

Efficient agent workflow (4 queries, ~500 tokens total):

```bash
# 1. Find the right interception point (~150 tokens)
al intercept "Sales-Post" --proc PostDocument --json

# 2. Get the procedure source to understand flow (~80 tokens)
al source "Sales-Post" --proc PostDocument --json

# 3. Check what the Customer table looks like (~120 tokens)
al object Customer --json

# 4. Check existing subscribers to avoid conflicts (~150 tokens)
al subscribers "Sales-Post" --json
```

**Without these tools**: Agent reads Sales-Post (~2000 lines), Customer table (~500 lines), scans workspace for EventSubscriber attributes (~all .al files) = **5000-10000 tokens** and multiple rounds of searching.

**With these tools**: **~500 tokens**, 4 targeted queries, agent has complete picture.

---

## Scenario 8: Dead Code Detection

**Agent question**: "Are there any unused procedures or fields in my extension?"

```bash
al dead-code --json
```
```json
{"unused":[
  {"k":"procedure","n":"OldHelper","obj":"My Codeunit","f":"src/MyCU.al","l":120,"reason":"zero references"},
  {"k":"field","id":50100,"n":"Legacy Flag","obj":"My Table","f":"src/MyTable.al","l":45,"reason":"zero references"},
  {"k":"subscriber","n":"OnOldEvent","obj":"My Sub","f":"src/MySub.al","l":80,"reason":"publisher not found in loaded packages"}
]}
```

**What the agent learns**: Unused code with reasons. "publisher not found" means the event was likely removed in a BC upgrade.

---

## Scenario 9: Agentic Debugging — Reproduce and Diagnose a Bug

**Agent task**: "Sales Order S-1001 posts with wrong amount. Debug and find where the value goes wrong."

```bash
# 1. Start a debug session
al debug start --json

# 2. Set breakpoints at key locations (informed by callgraph)
al debug breakpoint src/SalesProcessor.al 32 --json
al debug breakpoint src/SalesProcessor.al 55 --condition "SalesHeader.\"No.\" = 'S-1001'" --json

# 3. Trigger the posting (execution runs until breakpoint)
# ... BC client or test triggers the action ...

# 4. Inspect state when paused
al debug state --json
```
```json
{"cmd":"state","session":"s1","status":"paused",
"location":{"f":"src/SalesProcessor.al","l":32,"proc":"PostDocument"},
"stack":[
  {"frame":0,"proc":"PostDocument","obj":"Sales Processor","f":"src/SalesProcessor.al","l":32},
  {"frame":1,"proc":"OnRun","obj":"Sales Processor","f":"src/SalesProcessor.al","l":5}
],
"vars":[
  {"name":"SalesHeader","type":"Record \"Sales Header\"","fields":[
    {"n":"No.","v":"S-1001"},{"n":"Amount","v":"1500.00"},{"n":"Status","v":"Open"}
  ]},
  {"name":"IsHandled","type":"Boolean","v":"false"}
]}
```

```bash
# 5. Step through to see where amount changes
al debug step over --json
al debug eval "SalesHeader.Amount" --json
# {"cmd":"eval","expr":"SalesHeader.Amount","result":{"type":"Decimal","v":"1500.00"}}

al debug step over --json
al debug eval "SalesHeader.Amount" --json
# {"cmd":"eval","expr":"SalesHeader.Amount","result":{"type":"Decimal","v":"1200.00"}}
# ^ Amount changed — agent now knows this line caused the issue
```

```bash
# 6. Continue to next breakpoint, check final state
al debug continue --json

# 7. Review full history of this session
al debug history --var "SalesHeader.Amount" --json
```
```json
{"cmd":"history","session":"s1","hits":[
  {"seq":1,"bp":"bp1","time":"14:32:01.123",
   "location":{"f":"src/SalesProcessor.al","l":32,"proc":"PostDocument"},
   "vars":[{"name":"SalesHeader.Amount","v":"1500.00"}]},
  {"seq":2,"bp":"bp2","time":"14:32:01.890",
   "location":{"f":"src/SalesProcessor.al","l":55,"proc":"PostDocument"},
   "vars":[{"name":"SalesHeader.Amount","v":"1200.00"}]}
]}
```

```bash
# 8. Done — stop session
al debug stop --json
```

**What the agent learns**: The exact line and step where `Amount` changed from 1500 to 1200. Combined with `al source` to read that line, the agent can identify the bug without a human ever touching the debugger.

---

## Scenario 10: Agentic Debugging — Validate a Fix

**Agent task**: "I fixed the amount calculation. Verify it works by debugging S-1001 again."

```bash
# 1. Compile and start debug
al debug start --json

# 2. Set breakpoint after the fix location
al debug breakpoint src/SalesProcessor.al 55 --condition "SalesHeader.\"No.\" = 'S-1001'" --json

# 3. Wait for breakpoint hit, check the value
al debug state --json
al debug eval "SalesHeader.Amount" --json
# {"cmd":"eval","expr":"SalesHeader.Amount","result":{"type":"Decimal","v":"1500.00"}}
# ^ Amount is correct now — fix verified

# 4. Let it complete
al debug continue --json
al debug stop --json
```

**What the agent learns**: The fix works — same breakpoint, correct value. Agent can report "verified: Amount remains 1500.00 at line 55 for order S-1001" with evidence.

---

## Scenario 11: Agentic Debugging — Trace Variable Through Event Subscribers

**Agent task**: "Something is modifying SalesHeader after my code runs. Find which subscriber is changing it."

```bash
# 1. Use intercept to find events with var SalesHeader parameter
al intercept "Sales-Post" --field "Sales Header" --json
# Returns: OnBeforePostSalesDoc, OnAfterValidateHeader, etc.

# 2. Use subscribers to see who's listening
al subscribers "Sales-Post" --json

# 3. Set breakpoints at each subscriber entry point
al debug breakpoint src/MyExt.al 32 --json
al debug breakpoint src/ThirdPartyExt.al 100 --json

# 4. Start debug, hit first subscriber
al debug start --json
# ... execution pauses at subscriber ...

al debug eval "SalesHeader.\"Discount %\"" --json
# {"cmd":"eval","expr":"SalesHeader.\"Discount %\"","result":{"type":"Decimal","v":"0"}}

al debug continue --json
# ... hits next subscriber ...

al debug eval "SalesHeader.\"Discount %\"" --json
# {"cmd":"eval","expr":"SalesHeader.\"Discount %\"","result":{"type":"Decimal","v":"15"}}
# ^ This subscriber changed the discount!

al debug state --json
# Shows we're in ThirdPartyExt.OnBeforePostSalesDoc — that's the culprit
```

**What the agent learns**: Which event subscriber modified the record, without reading any code. Combines insight queries (intercept + subscribers) with debugging for a complete diagnosis.

---

## Test Expectations

Each scenario above is a test case.

### Insight Engine

1. **Return the exact schema shown** (field names, structure)
2. **Complete in <500ms** for workspace-scoped queries
3. **Handle cross-package tracing** (workspace → Base Application → System Application)
4. **Never return empty when data exists** — if Sales-Post has events, `al subscribers` must find them
5. **Include table operations accurately** — if a procedure calls `Customer.Modify()`, the table must appear with `modify` in ops
6. **Resolve transitive calls** — `al callgraph --depth 3` must follow calls across codeunit boundaries, not just within one object

### Agentic Debugger

7. **Session lifecycle**: start → breakpoint → pause → state/eval/step → continue → stop. Every transition returns valid JSON with `status` field.
8. **Variable inspection**: `state` returns all in-scope variables with current values. Record types expand to show field values.
9. **Expression evaluation**: `eval` supports field access (`Record.Field`), variable names, and simple expressions.
10. **Conditional breakpoints**: `--condition` expressions evaluated server-side, breakpoint only fires when true.
11. **History recording**: Every breakpoint hit records location + variable snapshot. `--var` filter returns only hits where that variable's value changed.
12. **Compilation before debug**: `start` compiles the project first. If compilation fails, returns error with diagnostics — never launches a debug session with stale code.
13. **Headless operation**: No UI required. Agent controls everything via CLI/MCP. The daemon manages the EditorServices.Host process lifecycle.
