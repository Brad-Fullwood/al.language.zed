; Outline/symbol rules for AL
; AUTO-GENERATED - do not edit manually

; Top-level object declarations
(object_declaration
  kind: (_) @context
  name: (_) @name) @item

; Procedures and methods
(procedure_declaration
  name: (_) @name) @item

; Triggers (OnRun, OnInsert, etc.)
(trigger_declaration
  name: (_) @name) @item

; Event declarations
(event_declaration
  name: (_) @name) @item

; Event procedure declarations
(event_procedure_declaration
  name: (_) @name) @item

; Key declarations
(key_declaration
  keyword: (_) @context
  name: (_) @name) @item

; Enum value declarations
(enum_value_declaration
  keyword: (_) @context
  name: (_) @name) @item

; Executable scopes. These nested items give the breadcrumb bar precise
; object > callable > block context instead of stopping at the procedure.
(begin_end_block
  (kw_begin) @name) @item

(if_statement
  (kw_if) @name) @item

(case_statement
  (kw_case) @name) @item

(for_statement
  (kw_for) @name) @item

(foreach_statement
  (kw_foreach) @name) @item

(while_statement
  (kw_while) @name) @item

(repeat_statement
  (kw_repeat) @name) @item

(with_statement
  (kw_with) @name) @item
