; Syntax-aware setting overrides for AL
; Disables auto-surround and auto-close inside comments and strings

[
  (comment)
] @comment.inclusive

[
  (string)
  (verbatim_string)
] @string
