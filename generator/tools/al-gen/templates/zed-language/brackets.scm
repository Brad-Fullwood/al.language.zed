; Bracket matching rules for AL
; AUTO-GENERATED - do not edit manually

; Standard bracket pairs
("[" @open "]" @close)
("(" @open ")" @close)

; begin/end blocks (procedures, triggers, etc.)
((begin_end_block (kw_begin) @open (kw_end) @close))

; if/then - kw_if opens, the begin_end_block's kw_end closes
((if_statement (kw_if) @open))

; case - kw_case opens, kw_end closes (kw_end is a direct child of case_statement)
((case_statement (kw_case) @open (kw_end) @close))

; repeat/until
((repeat_statement (kw_repeat) @open (kw_until) @close))

; while/do
((while_statement (kw_while) @open (kw_do) @close))

; for/do
((for_statement (kw_for) @open (kw_do) @close))

; foreach/do
((foreach_statement (kw_foreach) @open (kw_do) @close))
