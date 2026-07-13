# Prompt: add `break` / `continue` statements to tree-sitter-al

Hand this to an agent working in the **`Brad-Fullwood/AL-Tree-Sitter`** repo (the
`tree-sitter-al` submodule). The Rust interpreter side (`al-runtime`) already
handles `break_statement` / `continue_statement` nodes; it just needs the grammar
to emit them.

---

## Task

Business Central AL supports `break` and `continue` statements inside
`for` / `foreach` / `while` / `repeat` loops (added to the language ~2024).
The grammar currently declares `kw_break` and `kw_continue` only as reserved
keyword tokens — there are no statement productions — so `break;` / `continue;`
parse as bare identifier expressions (or errors). Add the two statements.

## Changes to `grammar.js`

1. Add two new rules modeled on the existing `exit_statement`:

   ```js
   break_statement: $ => $.kw_break,
   continue_statement: $ => $.kw_continue,
   ```

   (No arguments, no trailing semicolon — the `statement` production / caller
   already handles the `;` separator, exactly as `exit_statement` relies on it.)

2. Register both in the top-level `statement` choice, alongside
   `exit_statement`:

   ```js
   statement: $ => choice(
     $.begin_end_block,
     $.if_statement,
     $.empty_if_statement,
     $.case_statement,
     $.for_statement,
     $.foreach_statement,
     $.while_statement,
     $.repeat_statement,
     $.with_statement,
     $.asserterror_statement,
     $.exit_statement,
     $.break_statement,      // ← add
     $.continue_statement,   // ← add
     $.expression_statement,
   ),
   ```

   The `kw_break` / `kw_continue` tokens already exist in the reserved-keyword
   list, so no new token definitions are needed.

## Node shape the interpreter expects

- Node kind `break_statement` (child: `kw_break`)
- Node kind `continue_statement` (child: `kw_continue`)

That is all the interpreter's `eval_stmt` dispatch matches on
(`"break_statement" => Eval::Break`, `"continue_statement" => Eval::Continue`).

## Verify

1. `tree-sitter generate` succeeds with no new conflicts.
2. Add a corpus test (`test/corpus/`) so these parse as statements, e.g.:

   ```al
   codeunit 50100 T
   {
       procedure P()
       var i: Integer;
       begin
           for i := 1 to 10 do begin
               if i = 5 then break;
               if i = 2 then continue;
           end;
       end;
   }
   ```

   Expect `break_statement` and `continue_statement` nodes inside the loop body
   (not `expression_statement` / identifier).
3. `tree-sitter test` passes.

## After the grammar lands (consumer-side follow-up, not this repo)

In `al.language.zed`:
- Bump the `tree-sitter-al` submodule and the `extension.toml`
  `[grammars.al] rev` to the new grammar commit (they must match — see
  `CLAUDE.md` "Grammar rev drift").
- Add an end-to-end interpreter test in `al-runtime` (a loop with `break` /
  `continue` asserting BC-observed iteration counts) — this is currently blocked
  only by the grammar not emitting the nodes.
- Optionally add `break` / `continue` highlighting to `languages/al/highlights.scm`.
