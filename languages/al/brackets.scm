; Bracket matching rules for AL

("[" @open "]" @close)
("(" @open ")" @close)
("{" @open "}" @close)

; AL uses begin/end blocks extensively
((begin_end_block (kw_begin) @open (kw_end) @close))
