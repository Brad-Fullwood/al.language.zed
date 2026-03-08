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
] @indent

[
  "}"
  (kw_end)
  (kw_until)
] @outdent
