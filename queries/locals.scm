; Local scope and variable resolution for AL
; AUTO-GENERATED from node-types.json — do not edit manually

; SCOPES

(begin_end_block) @local.scope

(case_statement) @local.scope

(event_declaration) @local.scope

(for_statement) @local.scope

(foreach_statement) @local.scope

(if_statement) @local.scope

(object_declaration) @local.scope

(procedure_declaration) @local.scope

(repeat_statement) @local.scope

(source_file) @local.scope

(trigger_declaration) @local.scope

(while_statement) @local.scope

(with_statement) @local.scope

; DEFINITIONS

(object_declaration
  name: (name_or_keyword (name (identifier) @local.definition.type)))

(object_declaration
  name: (name_or_keyword (name (quoted_identifier) @local.definition.type)))

(procedure_declaration
  name: (name (identifier) @local.definition.method))

(procedure_declaration
  name: (name (quoted_identifier) @local.definition.method))

(trigger_declaration
  name: (name_or_keyword (name (identifier) @local.definition.method)))

(trigger_declaration
  name: (name_or_keyword (name (quoted_identifier) @local.definition.method)))

(event_declaration
  name: (name_or_keyword (name (identifier) @local.definition.method)))

(event_declaration
  name: (name_or_keyword (name (quoted_identifier) @local.definition.method)))

(regular_variable_declaration
  name: (name_or_keyword (name (identifier) @local.definition.var)))

(regular_variable_declaration
  name: (name_or_keyword (name (quoted_identifier) @local.definition.var)))

(parameter
  name: (name_or_keyword (name (identifier) @local.definition.parameter)))

(parameter
  name: (name_or_keyword (name (quoted_identifier) @local.definition.parameter)))

; REFERENCES

(identifier) @local.reference

(quoted_identifier) @local.reference

