//! Line and column conversion between BC's debugger positions and a DAP client.
//!
//! Standard route checked: DAP `initialize` reports `linesStartAt1` and
//! `columnsStartAt1`, and Microsoft's adapter applies them in
//! DebugHelper.cs:105-137 and Position.cs:82-100. This module follows those
//! rules so the editor path and the MCP path share one conversion.

use std::sync::atomic::{AtomicBool, Ordering};

/// What the client said in `initialize`. DAP defaults both to true.
#[derive(Debug)]
pub(crate) struct ClientBase {
    lines_start_at_1: AtomicBool,
    columns_start_at_1: AtomicBool,
}

impl Default for ClientBase {
    fn default() -> Self {
        Self {
            lines_start_at_1: AtomicBool::new(true),
            columns_start_at_1: AtomicBool::new(true),
        }
    }
}

impl ClientBase {
    /// Read `linesStartAt1` and `columnsStartAt1` from the `initialize` arguments.
    pub(crate) fn set_from_initialize(&self, arguments: &serde_json::Value) {
        let flag = |name: &str| {
            arguments
                .get(name)
                .and_then(|v| v.as_bool())
                .unwrap_or(true)
        };
        self.lines_start_at_1
            .store(flag("linesStartAt1"), Ordering::Relaxed);
        self.columns_start_at_1
            .store(flag("columnsStartAt1"), Ordering::Relaxed);
    }

    pub(crate) fn lines_start_at_1(&self) -> bool {
        self.lines_start_at_1.load(Ordering::Relaxed)
    }

    pub(crate) fn columns_start_at_1(&self) -> bool {
        self.columns_start_at_1.load(Ordering::Relaxed)
    }
}

/// BC `StatementSpan` line to the client's line: add 1 only when the client
/// counts from 1 and the line is above 0 (`ConvertClientLineToDebuggerLine`,
/// DebugHelper.cs:105-112, called at DebugAdapterStackTraceHandler.cs:101-102).
pub(crate) fn debugger_line_to_client(line: i64, lines_start_at_1: bool) -> i64 {
    if lines_start_at_1 && line > 0 {
        line.saturating_add(1)
    } else {
        line
    }
}

/// Same rule for columns (`ConvertClientColumnToDebuggerColumn`, DebugHelper.cs:114-121).
pub(crate) fn debugger_column_to_client(column: i64, columns_start_at_1: bool) -> i64 {
    if columns_start_at_1 && column > 0 {
        column.saturating_add(1)
    } else {
        column
    }
}

/// A client breakpoint line to BC's line: subtract 1 when the client counts
/// from 1 (`Position.Create`, Position.cs:92-100). Microsoft rejects line 0
/// from a one-based client as invalid, here it stays 0.
pub(crate) fn client_line_to_debugger(line: i64, lines_start_at_1: bool) -> i64 {
    if lines_start_at_1 {
        line.saturating_sub(1).max(0)
    } else {
        line
    }
}

/// How many lines BC's line for an object differs from the file's line, for
/// the object at `index` of a file that declares several. BC compiles each
/// object as its own compilation unit, the file's `using` and `namespace`
/// header followed by that object with its leading trivia, and counts
/// debugger lines in that unit. Microsoft's `GetDebuggerLineOffsetForNode`
/// (CodeAnalysis `SyntaxNodeExtensions`, decompiled from
/// Microsoft.Dynamics.Nav.CodeAnalysis.dll) returns `None` for a file with
/// fewer than two objects, otherwise the object's own start line plus the line
/// at which the first object's trivia starts. `DocumentHelper.cs:25-66` applies
/// it to breakpoint lines and `DebugAdapterStackTraceHandler.cs:80-101` takes
/// it off again. Both reduce to `debugger line = file line + shift`, in
/// zero-based lines, with
/// `shift = trivia start of the first object - trivia start of this object`.
/// An object's leading trivia starts on the line after the previous object's
/// closing line. The first object's starts after the last `using` or
/// `namespace` line before it, or at line 0.
///
/// `objects` holds the first and last zero-based line of each object in file
/// order. A file with one object has shift 0.
pub fn object_line_shift(source: &str, objects: &[(usize, usize)], index: usize) -> i64 {
    if objects.len() < 2 || index == 0 || index >= objects.len() {
        return 0;
    }
    let first_start = source
        .lines()
        .take(objects[0].0)
        .enumerate()
        .filter(|(_, text)| {
            let text = text.trim_start();
            text.starts_with("using ") || text.starts_with("namespace ")
        })
        .map(|(row, _)| row + 1)
        .last()
        .unwrap_or(0);
    let this_start = objects[index - 1].1 + 1;
    i64::try_from(first_start).unwrap_or(0) - i64::try_from(this_start).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_object_lines_are_relative_to_its_own_unit() {
        // Rows: 0 using, 1 blank, 2 table, 3 {, 4 }, 5 blank, 6 codeunit, 7 {, 8 trigger line, 9 }.
        let source = "using System;\n\ntable 50200 T\n{\n}\n\ncodeunit 50100 C\n{\n    x;\n}\n";
        let objects = [(2, 4), (6, 9)];
        assert_eq!(object_line_shift(source, &objects, 0), 0);
        // BC sees `using`, the blank line after the table, then the codeunit:
        // the statement on file row 8 is row 4 of that unit.
        let shift = object_line_shift(source, &objects, 1);
        assert_eq!(shift, 1 - 5);
        assert_eq!(8 + shift, 4);
        assert_eq!(object_line_shift(source, &objects[1..], 0), 0);
    }

    #[test]
    fn stack_lines_follow_convert_client_line_to_debugger_line() {
        // DebugHelper.cs:107: LinesStartAt1 && line > 0 adds 1, line 0 stays 0.
        assert_eq!(debugger_line_to_client(41, true), 42);
        assert_eq!(debugger_line_to_client(0, true), 0);
        assert_eq!(debugger_line_to_client(41, false), 41);
        assert_eq!(debugger_column_to_client(8, true), 9);
        assert_eq!(debugger_column_to_client(0, true), 0);
        assert_eq!(debugger_column_to_client(8, false), 8);
    }

    #[test]
    fn breakpoint_lines_follow_position_create() {
        // Position.cs:92-100: one-based input minus 1, zero-based input as is.
        assert_eq!(client_line_to_debugger(42, true), 41);
        assert_eq!(client_line_to_debugger(42, false), 42);
        assert_eq!(client_line_to_debugger(0, true), 0);
    }

    #[test]
    fn initialize_flags_default_to_one_based_and_can_be_cleared() {
        let base = ClientBase::default();
        assert!(base.lines_start_at_1() && base.columns_start_at_1());
        base.set_from_initialize(&serde_json::json!({
            "linesStartAt1": false,
            "columnsStartAt1": false
        }));
        assert!(!base.lines_start_at_1() && !base.columns_start_at_1());
        base.set_from_initialize(&serde_json::json!({}));
        assert!(
            base.lines_start_at_1(),
            "absent flag means DAP default true"
        );
    }
}
