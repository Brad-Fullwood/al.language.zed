# Grammar: an attributed member with a modifier loses its attribute after a var section

Found 2026-09-26 while testing event subscribers in the local interpreter.
Status: **open**. The fix is in the grammar repository (`tree-sitter-al`
submodule, Brad-Fullwood/AL-Tree-Sitter), which the session that found it
could not push to. The patch below is tested against this repository.

## Symptom

```al
codeunit 50199 "Tick Counter"
{
    var
        Calls: Integer;

    [EventSubscriber(ObjectType::Codeunit, Codeunit::Ticker, 'OnTick', '', false, false)]
    local procedure Count(var Seen: Integer)
    begin
    end;
}
```

The procedure parses without an `attribute` child or a `member_modifier`: a single
`kw_procedure` token spans `[EventSubscriber(...)]\n    local procedure`. Everything
that reads attributes from the tree misses them: the local interpreter's subscriber
index, the call graph's subscriber and publisher detection, and the test router,
which follows those edges. `[IntegrationEvent] local procedure` publishers after a
var section are lost the same way.

It needs all three: a var section just before the member, an attribute, and a
modifier word (`local`, `internal`, `protected`) after the attribute.
`[Test] procedure X()` after a var section parses correctly, which is why test
discovery never showed it.

## Cause

`scanner_scan_var_attribute_marker` (src/scanner.c) reads ahead over the
`[...]` groups and the next name to decide whether the attribute belongs to a
variable. When it decides it does not, it returns false with the lexer already
past `[...] local`, and `scan()` carries on to keyword scanning from there:
`procedure` is lexed as `KW_PROCEDURE` with the token starting at `[`.

## Fix

End the whole external scan when the marker lookahead has consumed input and
not produced a marker, so tree-sitter lexes from the `[` again:

```diff
diff --git a/src/scanner.c b/src/scanner.c
index 3945d7c..26d1164 100644
--- a/src/scanner.c
+++ b/src/scanner.c
@@ -837,7 +837,8 @@ static bool al_skip_name(TSLexer *lexer) {
 //
 // The token is zero width: it only steers the parser, and the `[` is still
 // consumed by the ordinary `attribute` rule.
-static bool scanner_scan_var_attribute_marker(TSLexer *lexer, const bool *valid_symbols) {
+static bool scanner_scan_var_attribute_marker(TSLexer *lexer, const bool *valid_symbols,
+                                              bool *consumed) {
   if (!valid_symbols[VAR_ATTRIBUTE_MARKER]) return false;
 
   while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r' ||
@@ -845,6 +846,8 @@ static bool scanner_scan_var_attribute_marker(TSLexer *lexer, const bool *valid_
     lexer->advance(lexer, true);
   }
   if (lexer->lookahead != '[') return false;
+  // From here the lexer reads ahead; a false result must end the whole scan.
+  *consumed = true;
 
   // Everything past this point is lookahead only.
   lexer->mark_end(lexer);
@@ -1132,9 +1135,15 @@ bool tree_sitter_al_external_scanner_scan(void *payload, TSLexer *lexer, const b
   }
 
 
-  if (scanner_scan_var_attribute_marker(lexer, valid_symbols)) {
+  bool marker_consumed = false;
+  if (scanner_scan_var_attribute_marker(lexer, valid_symbols, &marker_consumed)) {
     return true;
   }
+  // The marker lookahead read past `[...]` and any word after it. Scanning on
+  // from there would lex `[Attr] local procedure` as one keyword token.
+  if (marker_consumed) {
+    return false;
+  }
 
   if (valid_symbols[SIGNED_CASE_LABEL] && lexer->lookahead == '-') {
     lexer->advance(lexer, false);
```

With the patch the example parses as `(procedure_declaration (attribute ...)
(member_modifier (kw_local)) (kw_procedure) ...)`. This repository's whole test
suite passes against it (5,184 passed; the one failure is the root-only
`a_failed_file_write_leaves_the_whole_workspace_unchanged`). The grammar's own
corpus was not run. A corpus case for the example above belongs with the fix.

Once the submodule carries it, `subscriber_codeunits_with_globals_run_on_a_fresh_instance`
in `crates/al-runtime/src/interpreter/records_tests.rs` can move its var section
back above the subscriber.
