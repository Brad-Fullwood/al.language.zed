export const meta = {
  name: 'coverage-loop',
  description: 'Close red/green test gaps: pick the worst-covered production files, add REAL red-green tests (each verified to fail when the code is mutated/broken, pass when correct), re-measure coverage, commit + push. Drives toward complete, meaningful coverage — not vanity %.',
  whenToUse: 'Run repeatedly alongside the quality loop to raise genuine test coverage. Targets the gaps in .claude/coverage-gaps.md / live coverage data.',
  phases: [
    { title: 'Measure', detail: 'coverage run → ranked genuine gaps' },
    { title: 'WriteTests', detail: 'add red-green tests for the worst gaps (parallel authors)' },
    { title: 'Verify', detail: 'gates + each new test proven to bite' },
  ],
}

const RULES = `
PROJECT RULES (CLAUDE.md — non-negotiable):
- AL extension for Zed. Crates: al-core (logic + al-lsp binary), al-protocol, al-explorer, zed-al (WASM). ALWAYS exclude zed-al from cargo workspace commands; WASM build is separate.
- Gates: cargo fmt --all ; cargo check --workspace --exclude zed-al ; cargo clippy --workspace --exclude zed-al -- -D warnings ; cargo test --workspace --exclude zed-al ; cargo build -p zed-al --target wasm32-wasip1 --release.
- NEVER hardcode AL language values; use al_core::syntax::LanguageData / symbols / semantic.
- queries::* return transport-agnostic types, never lsp_types::* in signatures.
- UTF-16<->byte aware positions; iterative tree-sitter traversal; never hold a DashMap ref across await.
- Tests: test-only/dev deps (serial_test, wiremock) are pre-approved. Production deps need approval.
- Commit trailer (own line): Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
- Your VERY LAST action MUST be the StructuredOutput tool call. Do not end with prose.
`.trim()

// Reliability: retry so a transient StructuredOutput miss doesn't kill the run.
async function rAgent(prompt, opts, tries = 3) {
  for (let i = 1; i <= tries; i++) {
    try { return await agent(prompt, opts) }
    catch (e) { log(`retry ${opts && opts.label ? opts.label : 'agent'} (${i}/${tries}): ${String(e).slice(0, 110)}`) }
  }
  return null
}

const MEASURE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    overallLinePct: { type: 'number' },
    overallFnPct: { type: 'number' },
    gaps: {
      type: 'array',
      description: 'worst-covered GENUINE production gaps (exclude transport adapters that are only e2e-subprocess-tested: server/*.rs, bin/al-lsp.rs, thin clap dispatchers). Highest priority first.',
      items: {
        type: 'object',
        additionalProperties: false,
        properties: {
          file: { type: 'string' },
          linePct: { type: 'number' },
          why: { type: 'string', description: 'what is untested and whether it is unit-testable in-process' },
          testableInProcess: { type: 'boolean', description: 'false if it genuinely needs a spawned binary / live service (lower priority)' },
        },
        required: ['file', 'linePct', 'why', 'testableInProcess'],
      },
    },
  },
  required: ['overallLinePct', 'overallFnPct', 'gaps'],
}

const WRITE_SCHEMA = {
  type: 'object',
  additionalProperties: false,
  properties: {
    file: { type: 'string' },
    testsAdded: { type: 'integer' },
    commitSubject: { type: 'string' },
    committed: { type: 'boolean' },
    redGreenVerified: { type: 'boolean', description: 'true if you proved each new test FAILS when the code-under-test is deliberately broken and PASSES when restored' },
    coverageBefore: { type: 'number' },
    coverageAfter: { type: 'number' },
    notes: { type: 'string' },
  },
  required: ['file', 'testsAdded', 'commitSubject', 'committed', 'redGreenVerified', 'coverageBefore', 'coverageAfter', 'notes'],
}

let opts = args
if (typeof opts === 'string') { try { opts = JSON.parse(opts) } catch (e) { opts = null } }
if (!opts || typeof opts !== 'object') opts = {}
const maxFiles = opts.maxFiles || 3   // files to improve per run (sequential commits)

// ---- Measure: one coverage run → ranked genuine gaps ----
phase('Measure')
const measure = await rAgent(`${RULES}

Measure current test coverage and identify the worst GENUINE red/green gaps.

Run: \`scripts/coverage.sh\` (cargo-llvm-cov + nextest, ~2 min) OR \`cargo llvm-cov report --summary-only\` if a fresh run already exists in target/. Read .claude/coverage-gaps.md for prior analysis.

CRITICAL — distinguish two kinds of 0%/low coverage:
- Transport adapters (server/*.rs hover/definition/formatting/handlers/lsp/completions, bin/al-lsp.rs) show 0% ONLY because the e2e harness spawns al-lsp as a SUBPROCESS (uncounted). Their logic (queries/*) is covered. These are NOT priority gaps — mark testableInProcess=false.
- Genuine gaps = production logic with thin/no coverage that IS unit-testable in-process (e.g. http_auth.rs at 0% with zero tests, parsing/config helpers, symbol indexing). These are the targets.

Return overall % and the ranked genuine gaps (most-impactful, in-process-testable first).`,
  { label: 'measure coverage', phase: 'Measure', schema: MEASURE_SCHEMA, agentType: 'Explore' })

if (!measure) { log('Coverage measurement failed.'); return { error: 'measure failed' } }
const targets = (measure.gaps || []).filter((g) => g.testableInProcess).slice(0, maxFiles)
log(`Coverage: ${measure.overallLinePct}% line / ${measure.overallFnPct}% fn. Targeting ${targets.length} files: ${targets.map((t) => t.file.split('/').pop()).join(', ')}`)

if (targets.length === 0) {
  return { overallLinePct: measure.overallLinePct, overallFnPct: measure.overallFnPct, improved: 0, note: 'No in-process-testable gaps remain at this threshold — coverage is as complete as unit tests can make it.' }
}

// ---- Write tests: sequential (shared tree, each commits) ----
phase('WriteTests')
const results = []
for (let i = 0; i < targets.length; i++) {
  const t = targets[i]
  const r = await rAgent(`${RULES}

Add REAL red-green tests to raise coverage of ${t.file} (currently ${t.linePct}% line coverage).
Gap: ${t.why}

Requirements:
1. Read the file. Identify the untested functions / branches / error paths.
2. Write meaningful unit tests that exercise the real behavior — happy path AND error/edge paths (malformed input, boundary values, the documented failure modes). Match the file's existing test style; put them in the file's #[cfg(test)] mod (or a tests/ file if that's the crate's convention).
3. RED-GREEN PROOF (the whole point — no false positives): for the most important new test, TEMPORARILY break the code under test (flip a comparison, drop a guard, return a wrong value), run that test, and CONFIRM IT FAILS. Then restore the code and confirm it passes. A test that passes against broken code is worthless — do not keep it. Report redGreenVerified accordingly.
4. Do NOT test trivial getters or change production behavior. If a "gap" is genuinely not unit-testable in-process (needs a live BC server / spawned binary), add what you can and note the rest.
5. Run the FULL gate suite; make it green. Re-measure this file's coverage (\`cargo llvm-cov report --summary-only\` filtered to the file, or note the test count). Commit as ONE logical \`test(crate): cover <area>\` commit (with trailer). Push: \`git push\` (origin/dev); report the result, never --force.

Stay scoped to ${t.file}. Report exactly what you did.`,
    { label: `cover:${t.file.split('/').pop()}`, phase: 'WriteTests', schema: WRITE_SCHEMA })
  results.push(r)
  log(`${t.file.split('/').pop()}: +${r ? r.testsAdded : 0} tests, red-green ${r && r.redGreenVerified ? 'VERIFIED' : 'NOT verified'}`)
}

phase('Verify')
const done = results.filter(Boolean)
return {
  overallLinePctBefore: measure.overallLinePct,
  filesImproved: done.filter((r) => r.committed).length,
  totalTestsAdded: done.reduce((s, r) => s + (r.testsAdded || 0), 0),
  allRedGreenVerified: done.every((r) => r.redGreenVerified),
  perFile: done.map((r) => ({ file: r.file, tests: r.testsAdded, redGreen: r.redGreenVerified, cov: `${r.coverageBefore}→${r.coverageAfter}`, commit: r.commitSubject })),
  remainingGaps: (measure.gaps || []).filter((g) => g.testableInProcess).slice(maxFiles).map((g) => g.file),
}
