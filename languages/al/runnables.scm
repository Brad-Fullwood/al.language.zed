; Runnable detection for AL
; Detects [Test] procedures for run-button gutter icons

; Test procedures: [Test] procedure MyTest()
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "Test"))
  name: (name (identifier) @run)
  (#set! tag "al-test"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "Test"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-test"))
