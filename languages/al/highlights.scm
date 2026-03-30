; AL highlights for Zed/tree-sitter
; AUTO-GENERATED - DO NOT EDIT
; Keywords extracted from Microsoft.Dynamics.Nav.CodeAnalysis.dll

; --- Basic Literals ---
(comment) @comment
(string) @string
(verbatim_string) @string
(integer) @number
(decimal) @number
(date_literal) @number
(time_literal) @number
(datetime_literal) @number

; --- Preprocessor Directives & Inactive Code ---
(directive) @keyword.directive
(inactive_code) @comment

; --- Generic Identifier Fallback ---
; MUST be early so specific patterns below can override it
(identifier) @variable
; Quoted identifiers ("...") — in AL these are identifiers, NOT string literals
(quoted_identifier) @variable

; --- Boolean Literals ---
; true/false should be constants, not variables
((identifier) @constant.builtin
 (#match? @constant.builtin "^(true|false)$"))

; --- Keywords ---
; All keyword highlighting is dynamically generated from CodeAnalysis.dll categories
; --- Control Flow Keywords ---
; Zed themes color @keyword.control differently from @keyword
(kw_asserterror) @keyword.control
(kw_begin) @keyword.control
(kw_break) @keyword.control
(kw_case) @keyword.control
(kw_continue) @keyword.control
(kw_do) @keyword.control
(kw_downto) @keyword.control
(kw_else) @keyword.control
(kw_end) @keyword.control
(kw_exit) @keyword.control
(kw_for) @keyword.control
(kw_foreach) @keyword.control
(kw_if) @keyword.control
(kw_in) @keyword.control
(kw_of) @keyword.control
(kw_repeat) @keyword.control
(kw_then) @keyword.control
(kw_to) @keyword.control
(kw_until) @keyword.control
(kw_while) @keyword.control
(kw_with) @keyword.control

; --- Function Definition Keywords ---
(kw_event) @keyword.function
(kw_function) @keyword.function
(kw_procedure) @keyword.function
(kw_trigger) @keyword.function

; --- Modifier/Storage Keywords ---
(kw_indataset) @keyword.modifier
(kw_internal) @keyword.modifier
(kw_local) @keyword.modifier
(kw_protected) @keyword.modifier
(kw_runonclient) @keyword.modifier
(kw_suppressdispose) @keyword.modifier
(kw_temporary) @keyword.modifier
(kw_var) @keyword.modifier
(kw_withevents) @keyword.modifier

; --- Remaining Keywords ---
(kw_action) @keyword
(kw_actionref) @keyword
(kw_analysisview) @keyword
(kw_analysisviews) @keyword
(kw_array) @keyword
(kw_auditcategory) @keyword
(kw_automation) @keyword
(kw_biginteger) @keyword
(kw_bigtext) @keyword
(kw_blob) @keyword
(kw_boolean) @keyword
(kw_byte) @keyword
(kw_char) @keyword
(kw_clienttype) @keyword
(kw_code) @keyword
(kw_codeunit) @keyword
(kw_completiontriggererrorlevel) @keyword
(kw_connectiontype) @keyword
(kw_controladdin) @keyword
(kw_cookie) @keyword
(kw_customaction) @keyword
(kw_database) @keyword
(kw_dataclassification) @keyword
(kw_datascope) @keyword
(kw_datatransfer) @keyword
(kw_date) @keyword
(kw_dateformula) @keyword
(kw_datetime) @keyword
(kw_decimal) @keyword
(kw_defaultlayout) @keyword
(kw_dialog) @keyword
(kw_dictionary) @keyword
(kw_dotnet) @keyword
(kw_dotnetassembly) @keyword
(kw_dotnettypedeclaration) @keyword
(kw_duration) @keyword
(kw_entitlement) @keyword
(kw_enum) @keyword
(kw_enumextension) @keyword
(kw_errorinfo) @keyword
(kw_errortype) @keyword
(kw_executioncontext) @keyword
(kw_executionmode) @keyword
(kw_fieldclass) @keyword
(kw_fieldref) @keyword
(kw_fieldtype) @keyword
(kw_file) @keyword
(kw_fileupload) @keyword
(kw_fileuploadaction) @keyword
(kw_filterpagebuilder) @keyword
(kw_guid) @keyword
(kw_httpclient) @keyword
(kw_httpcontent) @keyword
(kw_httpheaders) @keyword
(kw_httprequestmessage) @keyword
(kw_httprequesttype) @keyword
(kw_httpresponsemessage) @keyword
(kw_instream) @keyword
(kw_integer) @keyword
(kw_interface) @keyword
(kw_isolationlevel) @keyword
(kw_joker) @keyword
(kw_jsonarray) @keyword
(kw_jsonobject) @keyword
(kw_jsontoken) @keyword
(kw_jsonvalue) @keyword
(kw_keyref) @keyword
(kw_list) @keyword
(kw_media) @keyword
(kw_mediaset) @keyword
(kw_moduledependencyinfo) @keyword
(kw_moduleinfo) @keyword
(kw_none) @keyword
(kw_notification) @keyword
(kw_notificationscope) @keyword
(kw_objecttype) @keyword
(kw_option) @keyword
(kw_outstream) @keyword
(kw_page) @keyword
(kw_pagebackgroundtaskerrorlevel) @keyword
(kw_pagecustomization) @keyword
(kw_pageextension) @keyword
(kw_pageresult) @keyword
(kw_pagestyle) @keyword
(kw_permissionset) @keyword
(kw_permissionsetextension) @keyword
(kw_profile) @keyword
(kw_profileextension) @keyword
(kw_program) @keyword
(kw_query) @keyword
(kw_record) @keyword
(kw_recordid) @keyword
(kw_recordref) @keyword
(kw_report) @keyword
(kw_reportextension) @keyword
(kw_reportformat) @keyword
(kw_secrettext) @keyword
(kw_securityfilter) @keyword
(kw_securityfiltering) @keyword
(kw_securityoperationresult) @keyword
(kw_sessionsettings) @keyword
(kw_systemaction) @keyword
(kw_table) @keyword
(kw_tableconnectiontype) @keyword
(kw_tableextension) @keyword
(kw_tablefilter) @keyword
(kw_testaction) @keyword
(kw_testfield) @keyword
(kw_testfilterfield) @keyword
(kw_testhttprequestmessage) @keyword
(kw_testhttpresponsemessage) @keyword
(kw_testpage) @keyword
(kw_testpermissions) @keyword
(kw_testrequestpage) @keyword
(kw_text) @keyword
(kw_textbuilder) @keyword
(kw_textconst) @keyword
(kw_textencoding) @keyword
(kw_time) @keyword
(kw_transactionmodel) @keyword
(kw_transactiontype) @keyword
(kw_value) @keyword
(kw_variant) @keyword
(kw_verbosity) @keyword
(kw_version) @keyword
(kw_view) @keyword
(kw_views) @keyword
(kw_webserviceactioncontext) @keyword
(kw_webserviceactionresultcode) @keyword
(kw_xmlattribute) @keyword
(kw_xmlattributecollection) @keyword
(kw_xmlcdata) @keyword
(kw_xmlcomment) @keyword
(kw_xmldeclaration) @keyword
(kw_xmldocument) @keyword
(kw_xmldocumenttype) @keyword
(kw_xmlelement) @keyword
(kw_xmlnamespacemanager) @keyword
(kw_xmlnametable) @keyword
(kw_xmlnode) @keyword
(kw_xmlnodelist) @keyword
(kw_xmlport) @keyword
(kw_xmlprocessinginstruction) @keyword
(kw_xmlreadoptions) @keyword
(kw_xmltext) @keyword
(kw_xmlwriteoptions) @keyword

; --- Keyword Operators ---
(op_and) @keyword.operator
(op_as) @keyword.operator
(op_div) @keyword.operator
(op_is) @keyword.operator
(op_mod) @keyword.operator
(op_not) @keyword.operator
(op_or) @keyword.operator
(op_xor) @keyword.operator

; Category captures for keyword types
(operator_word) @keyword.operator
(object_keyword) @keyword
(type_keyword) @type
(metadata_keyword) @keyword
(property_keyword) @keyword
(keyword) @keyword
; CRITICAL: control_keyword for keywords inside nested blocks (page triggers, etc.)
(control_keyword) @keyword.control

; --- Punctuation & Operators ---
(operator) @operator
(semicolon) @punctuation.delimiter
(comma) @punctuation.delimiter
["(" ")" "[" "]" "{" "}"] @punctuation.bracket

; =============================================================================
; STRUCTURAL PATTERNS (AST-based)
; These patterns understand syntax structure for richer highlighting.
; =============================================================================

; --- Object Declarations ---
; Highlight object names (codeunit "Name", table "Name", etc.)
; The name is nested: object_declaration > name: name_or_keyword > name > quoted_identifier
(object_declaration name: (name_or_keyword (name (quoted_identifier) @type)))
(object_declaration name: (name_or_keyword (name (identifier) @type)))
; Highlight the extends/implements target (direct quoted_identifier child via _pre_object_body)
(object_declaration (quoted_identifier) @type)

; --- Section/Control Names ---
; Names inside object_section headers: area(Content), group(General), action(CalcBalance), etc.
; The . anchor ensures only the first identifier (the name) is captured, not source expressions.
(object_section (parenthesized_block . (identifier) @type))
(object_section (parenthesized_block . (quoted_identifier) @type))

; --- Property Assignments ---
; Property names stay as their keyword captures (@keyword = teal), distinct from @type (green).
; Property values - identifiers like r, RIMD, All, true, false (after name: field)
(property_assignment
  name: (_)
  (name (identifier) @constant.builtin))
; Property values - table/object names in permissions (after name: field)
(property_assignment
  name: (_)
  (name (quoted_identifier) @type))

; --- Attributes ---
; Attribute names like [EventSubscriber(...)], [Test], etc.
(attribute name: (identifier) @attribute)

; --- Definitions ---
; Procedure, trigger, and event definition names
(procedure_declaration name: (name (identifier) @function.definition))
(procedure_declaration name: (name (quoted_identifier) @function.definition))
(trigger_declaration name: (_) @function.definition)
(event_declaration name: (_) @function.definition)

; --- Variable Declarations ---
; Variable names in declarations
(regular_variable_declaration name: (name_or_keyword (name (identifier) @variable.declaration)))
(regular_variable_declaration name: (name_or_keyword (name (quoted_identifier) @variable.declaration)))
(parameter name: (name_or_keyword (name (identifier) @variable.parameter)))
(parameter name: (name_or_keyword (name (quoted_identifier) @variable.parameter)))

; --- Type References ---
; Type names in variable declarations, parameters, and return types
(type_reference (name_or_keyword (name (identifier) @type)))
(type_reference (name_or_keyword (name (quoted_identifier) @type)))
(type_reference (qualified_name) @type)
(label_declaration type: (_) @type)

; --- Function Calls ---
; Direct function calls: FunctionName(args...)
(postfix_expression
  (primary_expression (name (identifier) @function.call))
  (call_suffix))
(postfix_expression
  (primary_expression (name (quoted_identifier) @function.call))
  (call_suffix))

; Method calls on objects: object.Method(args...)
(member_call_suffix member: (name (identifier) @function.method.call))
(member_call_suffix member: (name (quoted_identifier) @function.method.call))

; Scoped calls: Type::Method(args...)
(scope_call_suffix member: (name (identifier) @function.call))
(scope_call_suffix member: (name (quoted_identifier) @function.call))

; --- Scope References (non-call) ---
; Type::Member references (like ObjectType::Codeunit, Enum::Value)
(scope_suffix member: (name (identifier) @type))
(scope_suffix member: (name (quoted_identifier) @type))
