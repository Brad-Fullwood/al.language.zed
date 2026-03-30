; AL highlights for Zed/tree-sitter
; AUTO-GENERATED - DO NOT EDIT
; All basic token captures are dynamically extracted from the VS Code AL extension's TextMate grammar

; --- Basic Literals (dynamically extracted from TextMate scopes) ---
(comment) @comment
(string) @string
(verbatim_string) @string
(integer) @number
(decimal) @number
(date_literal) @number

; --- Generic Identifier Fallback ---
; MUST be early so specific patterns below can override it
(identifier) @variable
; Quoted identifiers ("...") - from TextMate identifier.quoted.double.al scope
(quoted_identifier) @variable

; --- Boolean Literals ---
; true/false should be constants, not variables
((identifier) @constant.builtin
 (#match? @constant.builtin "^(true|false)$"))

; --- Keywords ---
; All keyword highlighting is dynamically generated from TextMate grammar scopes
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

; Type keywords (override control keyword captures)
(kw_action) @type.builtin
(kw_actionref) @type.builtin
(kw_analysisview) @type.builtin
(kw_analysisviews) @type.builtin
(kw_array) @type.builtin
(kw_auditcategory) @type.builtin
(kw_automation) @type.builtin
(kw_biginteger) @type.builtin
(kw_bigtext) @type.builtin
(kw_blob) @type.builtin
(kw_boolean) @type.builtin
(kw_byte) @type.builtin
(kw_char) @type.builtin
(kw_clienttype) @type.builtin
(kw_code) @type.builtin
(kw_codeunit) @type.builtin
(kw_completiontriggererrorlevel) @type.builtin
(kw_connectiontype) @type.builtin
(kw_cookie) @type.builtin
(kw_customaction) @type.builtin
(kw_database) @type.builtin
(kw_dataclassification) @type.builtin
(kw_datascope) @type.builtin
(kw_datatransfer) @type.builtin
(kw_date) @type.builtin
(kw_dateformula) @type.builtin
(kw_datetime) @type.builtin
(kw_decimal) @type.builtin
(kw_defaultlayout) @type.builtin
(kw_dialog) @type.builtin
(kw_dictionary) @type.builtin
(kw_dotnet) @type.builtin
(kw_dotnetassembly) @type.builtin
(kw_dotnettypedeclaration) @type.builtin
(kw_duration) @type.builtin
(kw_enum) @type.builtin
(kw_errorinfo) @type.builtin
(kw_errortype) @type.builtin
(kw_executioncontext) @type.builtin
(kw_executionmode) @type.builtin
(kw_fieldclass) @type.builtin
(kw_fieldref) @type.builtin
(kw_fieldtype) @type.builtin
(kw_file) @type.builtin
(kw_fileupload) @type.builtin
(kw_fileuploadaction) @type.builtin
(kw_filterpagebuilder) @type.builtin
(kw_guid) @type.builtin
(kw_httpclient) @type.builtin
(kw_httpcontent) @type.builtin
(kw_httpheaders) @type.builtin
(kw_httprequestmessage) @type.builtin
(kw_httprequesttype) @type.builtin
(kw_httpresponsemessage) @type.builtin
(kw_instream) @type.builtin
(kw_integer) @type.builtin
(kw_interface) @type.builtin
(kw_isolationlevel) @type.builtin
(kw_joker) @type.builtin
(kw_jsonarray) @type.builtin
(kw_jsonobject) @type.builtin
(kw_jsontoken) @type.builtin
(kw_jsonvalue) @type.builtin
(kw_keyref) @type.builtin
(kw_list) @type.builtin
(kw_media) @type.builtin
(kw_mediaset) @type.builtin
(kw_moduledependencyinfo) @type.builtin
(kw_moduleinfo) @type.builtin
(kw_none) @type.builtin
(kw_notification) @type.builtin
(kw_notificationscope) @type.builtin
(kw_objecttype) @type.builtin
(kw_option) @type.builtin
(kw_outstream) @type.builtin
(kw_page) @type.builtin
(kw_pagebackgroundtaskerrorlevel) @type.builtin
(kw_pageresult) @type.builtin
(kw_pagestyle) @type.builtin
(kw_query) @type.builtin
(kw_record) @type.builtin
(kw_recordid) @type.builtin
(kw_recordref) @type.builtin
(kw_report) @type.builtin
(kw_reportformat) @type.builtin
(kw_secrettext) @type.builtin
(kw_securityfilter) @type.builtin
(kw_securityfiltering) @type.builtin
(kw_securityoperationresult) @type.builtin
(kw_sessionsettings) @type.builtin
(kw_systemaction) @type.builtin
(kw_table) @type.builtin
(kw_tableconnectiontype) @type.builtin
(kw_tablefilter) @type.builtin
(kw_testaction) @type.builtin
(kw_testfield) @type.builtin
(kw_testfilterfield) @type.builtin
(kw_testhttprequestmessage) @type.builtin
(kw_testhttpresponsemessage) @type.builtin
(kw_testpage) @type.builtin
(kw_testpermissions) @type.builtin
(kw_testrequestpage) @type.builtin
(kw_text) @type.builtin
(kw_textbuilder) @type.builtin
(kw_textconst) @type.builtin
(kw_textencoding) @type.builtin
(kw_time) @type.builtin
(kw_transactionmodel) @type.builtin
(kw_transactiontype) @type.builtin
(kw_variant) @type.builtin
(kw_verbosity) @type.builtin
(kw_version) @type.builtin
(kw_view) @type.builtin
(kw_views) @type.builtin
(kw_webserviceactioncontext) @type.builtin
(kw_webserviceactionresultcode) @type.builtin
(kw_xmlattribute) @type.builtin
(kw_xmlattributecollection) @type.builtin
(kw_xmlcdata) @type.builtin
(kw_xmlcomment) @type.builtin
(kw_xmldeclaration) @type.builtin
(kw_xmldocument) @type.builtin
(kw_xmldocumenttype) @type.builtin
(kw_xmlelement) @type.builtin
(kw_xmlnamespacemanager) @type.builtin
(kw_xmlnametable) @type.builtin
(kw_xmlnode) @type.builtin
(kw_xmlnodelist) @type.builtin
(kw_xmlport) @type.builtin
(kw_xmlprocessinginstruction) @type.builtin
(kw_xmlreadoptions) @type.builtin
(kw_xmltext) @type.builtin
(kw_xmlwriteoptions) @type.builtin


; Category captures dynamically generated from TextMate grammar scopes
(operator_word) @keyword.operator
(object_keyword) @keyword
(type_keyword) @type.builtin
(metadata_keyword) @keyword
(property_keyword) @operator
(keyword) @type.builtin
; CRITICAL: control_keyword for keywords inside nested blocks (page triggers, etc.)
(control_keyword) @keyword.control

; --- Punctuation & Operators (from TextMate punctuation.al scope) ---
(operator) @operator
(semicolon) @punctuation
(comma) @punctuation
["(" ")" "[" "]" "{" "}"] @punctuation.bracket

; =============================================================================
; STRUCTURAL PATTERNS (AST-based - cannot be derived from TextMate)
; These patterns understand syntax structure, which TextMate regex cannot.
; The CAPTURES use dynamically extracted scopes where applicable.
; =============================================================================

; --- Object Declarations ---
; Highlight object names (codeunit "Name", table "Name", etc.)
; The name is nested: object_declaration > name: name_or_keyword > name > quoted_identifier
(object_declaration name: (name_or_keyword (name (quoted_identifier) @title)))
(object_declaration name: (name_or_keyword (name (identifier) @title)))
; Highlight the extends/implements target (direct quoted_identifier child via _pre_object_body)
(object_declaration (quoted_identifier) @type)

; --- Property Assignments ---
; Property names in assignments like: Caption = 'value';
(property_assignment name: (_) @property)

; Property values - identifiers like r, RIMD, All, true, false (after name: field)
(property_assignment
  name: (_)
  (name (identifier) @constant.builtin))
; Property values - table/object names in permissions (after name: field)
(property_assignment
  name: (_)
  (name (quoted_identifier) @type.builtin))

; --- Attributes ---
; Attribute names like [EventSubscriber(...)], [Test], etc.
(attribute name: (identifier) @attribute)

; --- Definitions ---
; Procedure, trigger, and event definition names
(procedure_declaration name: (name (identifier) @function))
(procedure_declaration name: (name (quoted_identifier) @function))
(trigger_declaration name: (_) @function)
(event_declaration name: (_) @function)

; --- Variable Declarations ---
; Variable names in declarations
(regular_variable_declaration name: (name_or_keyword (name (identifier) @variable.declaration)))
(regular_variable_declaration name: (name_or_keyword (name (quoted_identifier) @variable.declaration)))
(parameter name: (name_or_keyword (name (identifier) @variable.parameter)))
(parameter name: (name_or_keyword (name (quoted_identifier) @variable.parameter)))

; --- Type References ---
; Type names in variable declarations, parameters, and return types
(type_reference (name_or_keyword (name (identifier) @type.builtin)))
(type_reference (name_or_keyword (name (quoted_identifier) @type.builtin)))
(type_reference (qualified_name) @type.builtin)
(label_declaration type: (_) @type.builtin)

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
(scope_suffix member: (name (identifier) @type.builtin))
(scope_suffix member: (name (quoted_identifier) @type.builtin))
