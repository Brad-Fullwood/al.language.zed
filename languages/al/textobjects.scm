; Text objects for AL (vim-mode: select function, class, comment)
; AUTO-GENERATED from node-types.json — do not edit manually

; Functions — procedures, triggers, events
(procedure_declaration) @function.around

(procedure_declaration
  (begin_end_block) @function.inside)

(trigger_declaration) @function.around

(trigger_declaration
  (begin_end_block) @function.inside)

(event_declaration) @function.around

(event_declaration
  (begin_end_block) @function.inside)

; Classes — AL objects (codeunit, table, page, report, etc.)
(object_declaration) @class.around

(object_declaration
  body: (object_body) @class.inside)

(object_section) @class.around

(object_section
  body: (object_body) @class.inside)

; Comments
(comment) @comment.around
