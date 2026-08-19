; Code folding regions for AL
; AUTO-GENERATED from node-types.json — do not edit manually

[
  (argument_list)
  (asserterror_statement)
  (begin_end_block)
  (braced_block)
  (case_branch)
  (case_statement)
  (enum_value_declaration)
  (event_declaration)
  (event_procedure_declaration)
  (for_statement)
  (foreach_statement)
  (if_statement)
  (key_declaration)
  (key_section)
  (object_declaration)
  (object_section)
  (object_var_section)
  (procedure_declaration)
  (repeat_statement)
  (trigger_declaration)
  (var_section)
  (while_statement)
  (with_statement)
] @fold

; Preprocessor regions
((directive) @fold.region.start
  (#match? @fold.region.start "^#[ \t]*[rR][eE][gG][iI][oO][nN]([ \t]|$)"))

((directive) @fold.region.end
  (#match? @fold.region.end "^#[ \t]*[eE][nN][dD][rR][eE][gG][iI][oO][nN]([ \t]|$)"))
