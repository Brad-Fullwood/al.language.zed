; Runnable detection for AL
; AUTO-GENERATED - do not edit manually

; [Test] procedures
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

; [TestPermissions(...)] procedures
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "TestPermissions"))
  name: (name (identifier) @run)
  (#set! tag "al-test"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "TestPermissions"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-test"))

; [EventSubscriber(...)] procedures
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "EventSubscriber"))
  name: (name (identifier) @run)
  (#set! tag "al-event-subscriber"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "EventSubscriber"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-event-subscriber"))

; [IntegrationEvent(...)] procedures (event publishers)
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "IntegrationEvent"))
  name: (name (identifier) @run)
  (#set! tag "al-event-publisher"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "IntegrationEvent"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-event-publisher"))

; [BusinessEvent(...)] procedures
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "BusinessEvent"))
  name: (name (identifier) @run)
  (#set! tag "al-event-publisher"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "BusinessEvent"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-event-publisher"))

; [HandlerFunctions('...')] procedures
(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "HandlerFunctions"))
  name: (name (identifier) @run)
  (#set! tag "al-test"))

(procedure_declaration
  (attribute
    name: (identifier) @_attr
    (#eq? @_attr "HandlerFunctions"))
  name: (name (quoted_identifier) @run)
  (#set! tag "al-test"))
