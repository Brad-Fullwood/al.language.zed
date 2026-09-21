# Blog inventory: technically-business-central

Read-only inventory of `/home/bradf/Projects/Personal/technically-business-central`, for planning the deletion/update of the six al.language.zed posts. Nothing in that repo was modified.

## 1. How the blog is wired

**Content collection** (`src/content.config.ts`): Astro Content Layer API, `glob({ pattern: '**/*.{md,mdx}', base: './src/content/blog' })`. Three collections total: `blog`, `pages`, `authors`.

**Blog frontmatter schema** (all fields):
- `title: string` (max 100 chars) - required
- `description: string` (max 200 chars) - required
- `publishedAt: date` (coerced) - required
- `updatedAt: date` (coerced) - optional
- `author: string` - optional, defaults to `'Team'`
- `image` (Astro image helper) - optional
- `imageAlt: string` - optional
- `tags: string[]` - optional, defaults to `[]`
- `project: string | string[]` - optional (union type, links a post to one or more entries in `src/data/projects.ts`)
- `readTime: number` - optional
- `draft: boolean` - optional, defaults to `false`
- `featured: boolean` - optional, defaults to `false`
- `locale: 'en' | 'es' | 'fr'` - optional, defaults to `'en'`

**Languages**: schema allows `en`, `es`, `fr`, but only `src/content/blog/en/` exists. No Spanish or French content anywhere in the repo.

**Images/heroes**: `image` is an optional Astro `image()` field on the post; `BlogCard.astro` renders it conditionally (`{image && ...}`). None of the six al.language.zed posts set `image`/`imageAlt`, so they render without a hero image and would not be visually orphaned by deletion.

**MDX usage**: only one post is `.mdx` — `al-explorer.mdx`. It imports and uses a single component, `TuiDemo` (`src/components/blog/TuiDemo.astro`), which renders a responsive iframe (desktop) or a "open in new tab" fallback (mobile) pointing at a static path (`src="/demos/al-explorer/"`).

**`tui-demos/` directory**: a standalone Rust/WASM project (`tui-demos/al-explorer/`), built with Trunk (`Trunk.toml`: `dist = "../../public/demos/al-explorer"`, `public_url = "/demos/al-explorer/"`). It's a hand-built ratatui recreation of the "AL Explorer" TUI, not the real al.language.zed `al-explorer` binary compiled to WASM — it's a separate demo app (`src/app.rs`, `src/data.rs`, `src/main.rs`, `src/ui.rs`) that loads real Microsoft symbol data fetched via `fetch-symbols.sh` (pulls `base-application` and `business-foundation` symbol packages from Microsoft's NuGet feed, extracts `SymbolReference.json`). The compiled output is checked into `public/demos/al-explorer/` (`.js` + `.wasm` + `index.html`) and served statically — this is what the `al-explorer.mdx` post's iframe embeds. Deleting the post orphans this demo (both `tui-demos/al-explorer/` and the compiled `public/demos/al-explorer/` files) unless something else references it.

## 2. The six posts (`src/content/blog/en/`)

All six share `publishedAt: 2026-03-14` and `author: "Brad Fullwood"`.

### `zed-al-extension.md` — "AL Development in Zed: Full Language Support Without VS Code"
1944 words. Feature-overview article: IntelliSense, diagnostics, code actions, formatting, semantic tokens, build/publish/debug, the Insight Engine, Linux support, settings/snippets, and Zed AI slash commands.

Concrete claims that could be stale:
- Perf numbers: hover <500μs, completions <3ms, workspace symbol search over 30,000 symbols <5ms.
- "10 quick fixes and 4 source actions."
- "16 semantic token types" (named list).
- Two-phase diagnostics with an 800ms debounce; setting name `zed-al.formatter.trailingNewline`.
- "59 settings," "23 snippet files."
- AI slash commands: `/al-explain`, `/al-refactor`, `/al-test`.
- "The source is on GitHub under the al-tools organization."
- Install flow: search "AL" in Zed's extension panel, downloads `al-lsp` binary from GitHub releases on first run.
- DashMap symbol index, `memmap2` parallel `.app` loading.

### `zed-al-architecture.md` — "Inside the Zed AL Extension: Architecture and Performance"
2407 words. Technical companion piece: crate structure, daemon model, workspace state, .NET bridge, symbol loading, init sequence, grammar pipeline, binary distribution, "what I got wrong," perf accounting.

Concrete claims that could be stale:
- **"The workspace is split into seven crates"**: `zed-al` (WASM) → `al-lsp` (binary) → `al-core` (lib) → `al-syntax`, `al-symbols`, `al-semantic`, plus three tool crates `al-cli`, `al-explorer`, `al-mcp`. This is a *different* crate count/list than `agentic-development.md`'s "12-crate Rust workspace" (see below) — the two posts already disagree with each other, before checking against the real repo.
- `al-explorer` described here as "file system watcher and workspace scanner" — contradicts `al-explorer.mdx`, which describes it as a TUI symbol browser. Internal inconsistency between posts.
- CI enforcement line: `cargo tree --edges no-dev | grep al-core`.
- Daemon socket path: `$XDG_RUNTIME_DIR/al-lsp-{workspace-hash}.sock`.
- `WorkspaceState` fields: `AlProject`, `DocumentStore`, `SymbolIndex`, `InsightEngine`, `SemanticBridge`, `DiagnosticStore`.
- .NET bridge: `netcorehost`, `AlBridge.dll`, `[UnmanagedCallersOnly]`, 2-second call timeout.
- Symbol loading: `memmap2`, one `rayon` thread per package, "BC base app symbol package is around 40MB."
- 11-step numbered initialization sequence.
- Grammar pipeline: `al-extract` → `al-gen` → `grammar.js` → `parser.c`, with a CI sync check.
- Binary distribution: checksum-verified GitHub release downloads, platform triples (`x86_64-unknown-linux-gnu`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`).
- "What I got wrong": `al-explorer` originally linked `al-core` directly, moved to the socket model; debounce tuning 300ms → 800ms.
- Perf numbers with specific benchmark setup (30,000 symbols / 500 documents, `criterion`): hover p99 <500μs, completions p99 <3ms, workspace symbol search p99 <5ms, cold load median <4s, incremental parse p99 <200μs.
- Named source paths: `al-symbols/src/loader.rs`, `al-semantic/src/inference.rs`, `al-core/src/bridge/mod.rs`, `AlBridge/src/AlBridge.cs`.
- Internal links to `/blog/zed-al-architecture` from other posts (see cross-links note below).

### `al-tree-sitter-grammar.md` — "Building a tree-sitter grammar for AL"
1637 words. Explains generating a tree-sitter grammar from `CodeAnalysis.dll` instead of hand-authoring, the C external scanner for case-insensitivity, preprocessor state handling, GLR conflicts, and WASM build constraints.

Concrete claims that could be stale:
- References prior art `SShadowS/tree-sitter-al` and its own repo `Brad-Fullwood/AL-Tree-Sitter` (`dev` branch, MIT), described as "a git submodule in the Zed AL extension."
- Tool names `al-extract` (C#/.NET reflection) and `al-gen` (Rust).
- Data table with exact counts: 159 keywords, 6 keyword categories, 313 properties, 127+ triggers, 158 builtin types.
- Preprocessor: 64-level if-stack, 1024-byte scanner serialization blob limit, `PreprocState` at 65 bytes.
- "`#define` used as a flag value... fail to parse — about 1.6% of files in practice."
- GLR: "18 conflicts total," parse time "under 5ms on a 5,000-line file."
- Parse rate: "98.4% of files in Microsoft's BCApps repo" (12,847 total parses, 203 failures).
- Integration: grammar submodule lives "under `al-syntax/`"; `build.rs` uses the `cc` crate to compile `parser.c`/`scanner.c`/`keywords.c`.
- Final `parser.c` size "around 2MB" (vs. the ~90MB problem in the competing grammar).

### `agentic-development.md` — "Declarative Agent Orchestration with Claude Code Plugins"
1865 words. Describes the `.claude/` methodology used to build the extension: rules, hookify enforcement, skills, subagents, `constraints.toml`.

Concrete claims that could be stale:
- Opens with **"The Zed AL Extension is a 12-crate Rust workspace"** — conflicts with `zed-al-architecture.md`'s "seven crates."
- Crate layout diagram: `al-lsp → al-core → al-syntax, al-symbols, al-semantic, al-diag`, with `al-protocol` as the only shared crate, plus `al-cli`, `al-explorer`, `al-mcp`, `zed-al` (WASM), `al-test-harness`. Different crate set again (`al-diag`, `al-protocol`, `al-test-harness` don't appear in the architecture post's list).
- `.claude/` directory layout: `agents/`, `skills/`, `rules/`, `hookify.*.local.md`, `constraints.toml`, `deferred-issues.toml`, `settings.json`.
- Table of seven named rule files (`architecture.md`, `code-boundaries.md`, `testing.md`, `thin-adapters.md`, `zed-fidelity.md`, `agentic-output.md`, `token-efficiency.md`).
- "Ten [hookify] rules. Eight guard architecture boundaries," plus two stop-event completion gates (`require-cargo-check-before-stop`, `require-cargo-test-before-stop`).
- Skills: `/start-work`, `/pof`, `/adversarial`, `/ci`, `/audit`, `/fix-infra`, `/report`.
- Example `proof_of_functionality.toml` entry claiming `tests_passed = 461, tests_failed = 0`.
- Three named subagents on Sonnet: Adversarial, Supervisor, Infra-fixer.
- Example `constraints.toml` contents naming specific hookify files and constraint IDs.
- "What I'd change" section calls out specific gaps (adversarial too eager, stop-hook ordering gap, 7 rule files → consolidate).
- This entire article's premise (a `.claude/`-based methodology with these specific rules/skills/agents) is itself a claim about the al.language.zed *development process*, not just the shipped product — worth checking against the current `.claude/` in al.language.zed, which per the campaign's own memory has since restructured (crate-split refactor, renamed to `al-lsp`).

### `al-explorer.mdx` — "AL Explorer: Browsing .app Packages Without the Wait"
851 words. Describes the TUI symbol browser: three-pane ratatui UI, connects to the `al-lsp` daemon over a Unix socket, embeds the interactive WASM demo via `TuiDemo`.

Concrete claims that could be stale:
- "A filtered search across 30,000+ symbols from 14 packages takes about 400 microseconds."
- "AL Explorer doesn't link against `al-core` or any of the analysis libraries... about 800 lines of ratatui layout code."
- "Compiles in under 3 seconds."
- Keyboard scheme: Tab (panes), `[`/`]` (cycle object kind), `/` (search), `g` (global search), double-click opens source in Zed.
- Virtual `.al` file generation for package (non-workspace) objects, read-only.
- Contradicts `zed-al-architecture.md`'s description of `al-explorer` as "a file system watcher and workspace scanner" (see above).

### `mcp-for-ai-context.md` — "AL CLI & MCP: Sub-Millisecond Code Intelligence for Humans and Agents"
1113 words. Compares the `al` CLI / `al-mcp` server against Microsoft's official AL MCP server.

Concrete claims that could be stale:
- "Microsoft's AL Language extension (version 18.0, March 2026) includes an MCP server via `launchmcpserver`... covers the build-publish-debug loop through 7 tools." (A specific, checkable claim about a *third-party* Microsoft product version/date.)
- Large feature-comparison table listing ~20 specific commands: `al build`, `al publish`, `al download-symbols`, `al search`, `al debug breakpoint`, `al debug` (7 sub-commands), `al snapshot`, `al object`, `al source`, `al events`, `al subscribers`, `al suggest-event`, `al callgraph`, `al trace`, `al intercept`, `al tables`, `al dead-code`, `al impact`, `al permissions --analyze`, `al debug eval`/`al debug history`, `al lint`, `al metrics`.
- "Every `al` command also exists as an MCP tool (`al/search`, `al/events`, etc.)... Add `--json` to any CLI command."
- Worked examples with fabricated-but-specific output: `al search`, `al impact "Sales Header"."Sell-to Customer No."`, `al dead-code --json`, a build-errors JSON blob, `al suggest-event`, and a full `al debug` session transcript.
- Daemon behavior: cold query "1-2 seconds," "auto-shuts down after 30 minutes idle."
- MCP config JSON example: `"command": "al-mcp", "args": ["--project", "/path/to/your/al-workspace"]`.
- Cross-links to `/blog/zed-al-architecture`.

**Cross-post links**: `agentic-development.md` links to `/blog/zed-al-architecture`; `mcp-for-ai-context.md` links twice to `/blog/zed-al-architecture`. If only some of the six posts are deleted, check for broken internal links; if all six go, this is moot.

## 3. Places outside the posts that reference a post slug or the project

- **RSS (`src/pages/rss.xml.ts`)** and **OG images (`src/pages/og/[...slug].png.ts`)**: both generate entries dynamically from `getCollection('blog', ...)`. No hardcoded slug list — deleting the posts removes them from RSS/OG automatically, nothing to edit.
- **Sitemap**: `@astrojs/sitemap` integration in `astro.config.mjs`, auto-generated from the route tree at build time. Nothing hardcoded to update.
- **Home page (`src/pages/index.astro`)**: pulls "Recent Articles" dynamically (`getCollection('blog', ...).slice(0, 4)`) — no hardcoded slugs, updates itself once posts are gone. It also renders a "Featured Projects" section from `src/data/projects.ts` (see below), independent of the blog posts.
- **Nav (`src/config/nav.config.ts`)**: static top-level nav (`Blog`, `Projects`, `About`, `Contact`) — no post-specific entries.
- **`vercel.json`**: only security headers, no `redirects`/`rewrites` referencing post slugs.
- **`src/data/projects.ts`**: the real place that needs manual attention. Three `featured: true` project entries directly describe the al.language.zed project family and its Insight Engine, Linux support, "actually works on Linux" framing, etc., independent of the blog text:
  - `al-tools` (slug) — "Tree-sitter grammar, CLI, MCP server, TUI explorer, and LSP..."
  - `zed-al-extension` (slug) — "Full AL language support in Zed. Native Rust LSP, Insight Engine for cross-extension intelligence, and it actually works on Linux."
  - `ai-assisted-dev` (slug) — "Claude Code setup with specialist agents, enforcement hooks, and MCP-powered context," names "6 specialist sub-agents (adversarial, auditor, guardian, supervisor, team-lead, infra-fixer)" — yet another crate/agent-count claim inconsistent with `agentic-development.md`'s "three subagents."
  - `getProjectArticles()` in this file filters posts by `project` slug for display on `/projects/[slug]` pages; once the six posts are deleted, these three project pages will simply show zero articles (no crash — the function just returns an empty filtered array), but the pages remain live with their own separate stale claims above.
- **`src/pages/projects/[slug].astro`**: renders project detail pages from `projects.ts`, including whatever articles `getProjectArticles` finds. No separate hardcoded content to fix beyond `projects.ts` itself.
- **`src/components/ui/marketing/TerminalDemo/TerminalDemo.tsx`** (used on the home page hero, `client:visible`): hardcodes a simulated terminal session including the literal line `$ al-explorer --workspace .` and a terminal header labelled `al-explorer`. This is independent of the blog posts and will need updating/removing separately.
- **`tui-demos/al-explorer/`** and its compiled output **`public/demos/al-explorer/`**: only consumer is `al-explorer.mdx`'s `TuiDemo` import. Orphaned once that post is deleted — delete or repurpose.
- **`.claude/skills/article-writer/SKILL.md`**: names the three featured project slugs (`al-tools, zed-al-extension, ai-assisted-dev`) under "Project context," and states "al.language.zed is on branch `dev` (current HEAD, ~130 commits past v0.2.2)" as the research baseline — this is now stale relative to whatever al.language.zed's actual `dev` HEAD is.
- **`README.md`**: no post-specific or al.language.zed-specific content — generic stack/author blurb only.
- **`.claude/agents/article-writer.md`**: a subagent definition (model: sonnet) that duplicates the skill's process/voice content almost verbatim, with an added `Agent`/`Skill` tool grant. No slug references beyond the same project-context blurb as the skill.
- No `src/content/pages/` or `about.astro`/`contact.astro` references to the six posts turned up in the slug grep.

## 4. Build and check locally

- Package manager: **pnpm** (`pnpm-lock.yaml`, `pnpm-workspace.yaml`, `package.json` has a `"pnpm"` field, `engines.node >=22.12.0`).
- `node_modules` is **not present** — `pnpm install` has not been run in this checkout.
- `package.json` scripts:
  - `dev` — `astro dev`
  - `build` — `astro build && npx pagefind --site dist`
  - `preview` — `astro preview`
  - `check` — `astro check` (typecheck)
  - `lint` — `eslint .`
  - `lint:fix` — `eslint . --fix`
  - `format` / `format:check` — `prettier`
  - `validate` — `pnpm lint && pnpm check && pnpm build` (the one-shot gate)
  - `test` — `vitest`
  - `test:e2e` — `playwright test`
- ESLint config (`eslint.config.js`): flat config, `@eslint/js` recommended + `typescript-eslint` recommended + `eslint-plugin-astro` recommended, ignores `dist/`, `node_modules/`, `.astro/`, `public/pagefind/`.
- To verify anything locally you'd need `pnpm install` first; not run here since this was read-only.

## 5. Writing conventions (`article-writer` + `humanizer` skills, 10 lines)

- `article-writer`: writes as "Brad Fullwood," first person, for BC developers; four-step process — draft with required frontmatter shape, mandatory `humanizer` pass, a self-check "what still looks AI-generated" pass, then a final read for code accuracy and frontmatter validity.
- Voice: specific and technical, code/output/numbers over description, allowed to have opinions and reactions, varied sentence length, no corporate tone, honest about tradeoffs and limits.
- Explicit content rules: never mention career transitions or job hunting, never use "delve," frame the project honestly (confident about what's native/fast, explicit about what falls back to Microsoft's compiler/runtime), never claim a feature not actually in the code, code examples must be real.
- Told to research "from the live working tree" — al.language.zed `dev` branch (a specific, now possibly stale, commit baseline) and AL-Tree-Sitter `dev` — verifying against source, not aspirational docs.
- `humanizer`: mandatory post-draft pass that rewrites prose in place (frontmatter/code/MDX tags untouched) to strip LLM tells.
- Long kill-list: corporate vocabulary (delve, leverage, robust, seamless, streamline, empower, etc.), "not just X but Y," rule-of-three triads, both-sides hedge paragraphs, throat-clearing intros/summary outros, editorializing section-enders, connective overuse (moreover/furthermore/thus), hedging language, uniform paragraph texture, em-dash tics, "whether you're A or B" audience-fanning.
- What to add back: concrete specifics (real commands/settings/paths/numbers), asymmetric personal reactions, honest limitations, real reasons behind choices rather than generic benefits.
- Test given to the humanizer: read it aloud — if it sounds like a landing page or conference abstract, it fails; if it sounds like a developer explaining something they built to another developer over coffee, it passes.
- Humanizer explicitly does not touch technical claims, code, or numbers — that's out of scope for it ("that's the fact-checker's job"), meaning the six posts' many specific numeric/architectural claims were never fact-checked by this pipeline, only voice-edited.

## 6. Deployment

- **`vercel.json`**: security headers only (CSP, Permissions-Policy, Referrer-Policy, X-Content-Type-Options, X-Frame-Options, X-XSS-Protection, Cache-Control, Strict-Transport-Security). No redirects, no rewrites, no build overrides.
- **Git remote**: `origin` → `https://github.com/Brad-Fullwood/technically-business-central` (fetch and push).
- **Current branch**: `main`.
- **README** states the live site is `https://technically-business-central.vercel.app`, deployed to Vercel; matches `astro.config.mjs`'s default `site` value (`process.env.SITE_URL || 'https://technically-business-central.vercel.app'`).
- `.github/` contains only `dependabot.yml`, no workflow files — no GitHub Actions build/deploy pipeline. Deployment is Vercel's standard git-integration auto-deploy on push to `main` (nothing in-repo runs `pnpm build`/`pnpm validate` as a gate before deploy).

## Inventory complete
