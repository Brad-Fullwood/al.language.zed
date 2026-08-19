; Indentation rules for AL

[
  (object_declaration)
  (procedure_declaration)
  (trigger_declaration)
  (if_statement)
  (case_statement)
  (repeat_statement)
  (while_statement)
  (for_statement)
  (foreach_statement)
  (braced_block)
  (begin_end_block)
  (var_section)
  (object_var_section)
] @indent

[
  "}"
  (kw_end)
  (kw_until)
] @outdent

; Only IF takes an outdented ELSE. In a CASE statement the ELSE arm sits at the
; same level as the case labels, which is what the (case_statement) @indent
; above already produces.
(if_statement (kw_else) @outdent)
