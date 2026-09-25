; Outline/symbol rules for AL

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
;
; The keyword is the context and the block's own expression is the name, so a
; breadcrumb reads `Post > if not Item.IsEmpty()`. Naming every block after its
; bare keyword would fill the outline, and the fuzzy symbol search that reads
; it, with rows called `if` and `begin`. A `begin` block has no expression of
; its own and keeps the keyword.
(begin_end_block
  (kw_begin) @name) @item

(if_statement
  (kw_if) @context
  condition: (_) @name) @item

(case_statement
  (kw_case) @context
  value: (_) @name) @item

(for_statement
  (kw_for) @context
  iterator: (_) @name) @item

(foreach_statement
  (kw_foreach) @context
  iterator: (_) @name) @item

(while_statement
  (kw_while) @context
  condition: (_) @name) @item

(repeat_statement
  (kw_repeat) @context
  condition: (_) @name) @item

(with_statement
  (kw_with) @context
  value: (_) @name) @item
