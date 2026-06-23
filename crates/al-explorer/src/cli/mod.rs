//! AL CLI mode — thin JSON-RPC client for the al-lsp daemon.
//!
//! All business logic lives in the daemon. This module parses CLI arguments,
//! connects to the daemon, sends JSON-RPC requests, and formats responses for
//! human or `--json` output. Folded into al-explorer in stage 8 of the crate
//! consolidation; previously the standalone `al-cli` crate.
//!
//! The argument types (`Cli`, `Commands`, and the nested subcommand enums) live
//! in the `args` and `subcommands` submodules and are re-exported here; this
//! file keeps the command routing (`run`).

// edition 2024 turns collapsible_if into a hard error; the migrated code
// pre-dates that and the patterns are intentional for readability.
#![allow(clippy::collapsible_if)]

pub mod args;
pub mod commands;
pub mod subcommands;

pub use args::*;
pub use subcommands::*;

use std::process::ExitCode;

use clap::CommandFactory;
use clap_complete::generate;

use commands::{build, debug, insight, lsp};

// Entry point — invoked from al-explorer's main when CLI args are present.

pub fn run(cli: Cli) -> ExitCode {
    match cli.command {
        Commands::GenerateCompletions { shell } => {
            let mut cmd = Cli::command();
            generate(shell, &mut cmd, "al-explorer", &mut std::io::stdout());
            ExitCode::SUCCESS
        }
        Commands::Version => lsp::cmd_version(cli.json),
        Commands::ClearCache => lsp::cmd_clear_cache(cli.json),
        Commands::Setup => lsp::cmd_setup(cli.json),
        Commands::Doctor => lsp::cmd_doctor(cli.json),
        Commands::DownloadSymbols { project, source } => {
            lsp::cmd_download_symbols(project.as_deref(), source.as_deref(), cli.json)
        }
        Commands::Search { query, limit } => lsp::cmd_search(&query, limit, cli.json),
        Commands::Object { kind, name } => lsp::cmd_object(&kind, &name, cli.json),
        Commands::ById { kind, id } => lsp::cmd_by_id(&kind, id, cli.json),
        Commands::Events { name } => lsp::cmd_events(&name, cli.json),
        Commands::Subscribers { event } => lsp::cmd_subscribers(&event, cli.json),
        Commands::EventSource { file, line } => lsp::cmd_event_source(&file, line, cli.json),
        Commands::Composed { kind, name } => lsp::cmd_composed(&kind, name.as_deref(), cli.json),
        Commands::Packages => lsp::cmd_packages(cli.json),
        Commands::Deps => lsp::cmd_deps(cli.json),
        Commands::Compile { project } => build::cmd_compile(project.as_deref(), cli.json),
        Commands::PackNative { project, out } => {
            build::cmd_pack_native(project.as_deref(), out.as_deref(), cli.json)
        }
        Commands::Lint {
            file,
            all,
            analyzers,
        } => {
            let joined = if file.is_empty() {
                None
            } else {
                Some(file.join(" "))
            };
            lsp::cmd_lint(joined.as_deref(), all, analyzers.as_deref(), cli.json)
        }
        Commands::Format {
            file,
            check,
            stdin,
            all,
        } => lsp::cmd_format(file.as_deref(), check, stdin, all, cli.json),
        Commands::Symbols { file } => lsp::cmd_symbols(&file, cli.json),
        Commands::Hover { file, line, col } => lsp::cmd_hover(&file, line, col, cli.json),
        Commands::Definition { file, line, col } => {
            lsp::cmd_position_query("definition", &file, line, col, cli.json)
        }
        Commands::References { file, line, col } => {
            lsp::cmd_position_query("references", &file, line, col, cli.json)
        }
        Commands::Signature { file, line, col } => {
            lsp::cmd_position_query("signatureHelp", &file, line, col, cli.json)
        }
        Commands::Completions { file, line, col } => {
            lsp::cmd_position_query("completions", &file, line, col, cli.json)
        }
        Commands::Rename {
            file,
            line,
            col,
            new_name,
            dry_run,
        } => lsp::cmd_rename(&file, line, col, &new_name, dry_run, cli.json),
        Commands::Rules => lsp::cmd_rules(cli.json),
        Commands::ErrorCodes => lsp::cmd_error_codes(cli.json),
        Commands::Builtins => lsp::cmd_builtins(cli.json),
        Commands::Folding { file } => lsp::cmd_folding(&file, cli.json),
        Commands::Tokens { file } => lsp::cmd_tokens(&file, cli.json),
        Commands::Parse { file } => lsp::cmd_parse(&file, cli.json),
        Commands::Metrics {
            file,
            all,
            threshold_cyclomatic,
            threshold_cognitive,
        } => lsp::cmd_metrics(
            file.as_deref(),
            all,
            threshold_cyclomatic,
            threshold_cognitive,
            cli.json,
        ),
        Commands::SqlScan => lsp::cmd_sql_scan(cli.json),
        Commands::Hints {
            file,
            start_line,
            end_line,
        } => lsp::cmd_hints(&file, start_line, end_line, cli.json),
        Commands::Fix {
            file,
            dry_run,
            rule,
        } => lsp::cmd_fix(file.as_deref(), dry_run, rule.as_deref(), cli.json),
        Commands::Permissions {
            format,
            name,
            id,
            role_id,
        } => lsp::cmd_permissions(&format, &name, id, &role_id, cli.json),
        Commands::Package => build::cmd_package(cli.json),
        Commands::New {
            dir,
            name,
            publisher,
            template,
        } => lsp::cmd_new(&dir, &name, &publisher, &template, cli.json),
        Commands::InitDebug => lsp::cmd_init_debug(&commands::project_root(None), cli.json),
        Commands::Authenticate { cmd, tenant } => {
            lsp::cmd_authenticate(&cmd, tenant.as_deref(), cli.json)
        }
        Commands::Trace { event, depth, tree } => insight::cmd_trace(&event, depth, tree, cli.json),
        Commands::Intercept => insight::cmd_intercept(cli.json),
        Commands::Entrypoints => insight::cmd_entrypoints(cli.json),
        Commands::Graph { format } => insight::cmd_graph(&format, cli.json),
        Commands::InsightStats => insight::cmd_insight_stats(cli.json),
        Commands::DeadCode => insight::cmd_dead_code(cli.json),
        Commands::Impact { symbol, table } => insight::cmd_impact(&symbol, table, cli.json),
        Commands::SuggestEvent {
            object,
            procedure,
            table,
            field,
            event,
        } => insight::cmd_suggest_event(object, procedure, table, field, event, cli.json),
        Commands::Diag => lsp::cmd_diag(cli.json),
        Commands::Debug { subcmd } => debug::cmd_debug(&subcmd, cli.json),
        Commands::Snapshot { subcmd } => debug::cmd_snapshot(&subcmd, cli.json),
        Commands::Profile { subcmd } => debug::cmd_profile(&subcmd, cli.json),
        Commands::Xlf { subcmd } => build::cmd_xlf(&subcmd, cli.json),
        Commands::AddApplicationArea { value, dry_run } => {
            lsp::cmd_add_application_area(&value, dry_run, cli.json)
        }
        Commands::AddTooltips {
            from_table,
            dry_run,
        } => lsp::cmd_add_tooltips(from_table.as_deref(), dry_run, cli.json),
        Commands::AddDataClassification { value, dry_run } => {
            lsp::cmd_add_data_classification(&value, dry_run, cli.json)
        }
        Commands::Tests => lsp::cmd_tests_discover(cli.json),
        Commands::TestRun {
            codeunit,
            name,
            method,
            config,
        } => lsp::cmd_test_run(
            codeunit,
            name.as_deref(),
            method.as_deref(),
            config.as_deref(),
            cli.json,
        ),
        Commands::TestCoverage => lsp::cmd_tests_coverage(cli.json),
        Commands::TestAffected { files } => lsp::cmd_test_affected(&files, cli.json),
        Commands::TestClassify => lsp::cmd_test_classify(cli.json),
        Commands::TestResults { codeunit, method } => {
            lsp::cmd_test_results(codeunit, method.as_deref(), cli.json)
        }
        Commands::TestSnapshot { subcmd } => lsp::cmd_test_snapshot(&subcmd, cli.json),
        Commands::TestMutate {
            files,
            parallel,
            timeout_ms,
        } => lsp::cmd_test_mutate(&files, parallel, timeout_ms, cli.json),
        Commands::TestRunAll {
            parallel,
            timeout_ms,
            junit_out,
            cobertura_out,
            filter,
        } => lsp::cmd_test_run_all(
            parallel,
            timeout_ms,
            junit_out.as_deref(),
            cobertura_out.as_deref(),
            filter.as_deref(),
            cli.json,
        ),
        Commands::Generate {
            kind,
            id,
            name,
            table,
            page_type,
            subject,
        } => lsp::cmd_generate(
            &kind,
            id,
            &name,
            table.as_deref(),
            page_type.as_deref(),
            subject.as_deref(),
            cli.json,
        ),
        Commands::Obsolete => lsp::cmd_obsolete(cli.json),
        Commands::AuditData => lsp::cmd_audit_data_classification(cli.json),
        Commands::PermissionAudit => lsp::cmd_permission_audit(cli.json),
        Commands::DepsGraph { format } => lsp::cmd_deps_graph(&format, cli.json),
        Commands::Breaking => lsp::cmd_breaking_changes(cli.json),
        Commands::ArchLint => lsp::cmd_arch_lint(cli.json),
        Commands::Duplicates {
            min_tokens,
            min_similarity,
        } => lsp::cmd_duplicates(min_tokens, min_similarity, cli.json),
        Commands::Upgrade => lsp::cmd_upgrade_report(cli.json),
        Commands::ProfilerHints { hotspots } => lsp::cmd_profiler_hints(&hotspots, cli.json),
        Commands::SortMembers { file, all, dry_run } => {
            lsp::cmd_sort_members(file.as_deref(), all, dry_run, cli.json)
        }
        Commands::OrganizeFiles { dry_run } => lsp::cmd_organize_files(dry_run, cli.json),
    }
}
