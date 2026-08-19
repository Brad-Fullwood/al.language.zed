; Bracket matching rules for AL

; Standard bracket pairs
("[" @open "]" @close)
("(" @open ")" @close)
("{" @open "}" @close)

; begin/end blocks (procedures, triggers, and nested statement blocks)
((begin_end_block (kw_begin) @open (kw_end) @close))

; case/end (kw_end is a direct child of case_statement)
((case_statement (kw_case) @open (kw_end) @close))

; repeat/until
((repeat_statement (kw_repeat) @open (kw_until) @close))

; while/do
((while_statement (kw_while) @open (kw_do) @close))

; for/do
((for_statement (kw_for) @open (kw_do) @close))

; foreach/do
((foreach_statement (kw_foreach) @open (kw_do) @close))
