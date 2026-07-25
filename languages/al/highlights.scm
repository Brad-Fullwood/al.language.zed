; AUTO-GENERATED - DO NOT EDIT

; Literals
(comment) @comment
(string) @string
(verbatim_string) @string
(integer) @number
(decimal) @number
(date_literal) @number

; Generic captures precede structural overrides because query matches are last-wins.
(identifier) @variable
(quoted_identifier) @variable

((identifier) @constant.builtin
 (#match? @constant.builtin "^(true|false)$"))

; Keywords
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

(kw_event) @keyword.function
(kw_function) @keyword.function
(kw_procedure) @keyword.function
(kw_trigger) @keyword.function

(kw_indataset) @keyword.modifier
(kw_internal) @keyword.modifier
(kw_local) @keyword.modifier
(kw_protected) @keyword.modifier
(kw_runonclient) @keyword.modifier
(kw_suppressdispose) @keyword.modifier
(kw_temporary) @keyword.modifier
(kw_var) @keyword.modifier
(kw_withevents) @keyword.modifier

(kw_controladdin) @keyword
(kw_entitlement) @keyword
(kw_enumextension) @keyword
(kw_pagecustomization) @keyword
(kw_pageextension) @keyword
(kw_permissionset) @keyword
(kw_permissionsetextension) @keyword
(kw_profile) @keyword
(kw_profileextension) @keyword
(kw_program) @keyword
(kw_reportextension) @keyword
(kw_tableextension) @keyword
(kw_value) @keyword

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


(operator_word) @keyword.operator
(object_keyword) @keyword
(type_keyword) @type.builtin
(metadata_keyword) @keyword
(property_keyword) @operator
(keyword) @keyword
(control_keyword) @keyword.control

; Punctuation and operators
(operator) @operator
(semicolon) @punctuation
(comma) @punctuation
["(" ")" "[" "]" "{" "}"] @punctuation.bracket

; Object declarations
(object_declaration name: (name_or_keyword (name (quoted_identifier) @title)))
(object_declaration name: (name_or_keyword (name (identifier) @title)))
(object_declaration (quoted_identifier) @type)

; Properties
(property_assignment name: (_) @property)

(property_assignment
  name: (_)
  (name (identifier) @constant.builtin))
(property_assignment
  name: (_)
  (name (quoted_identifier) @type.builtin))

; Attributes
(attribute name: (identifier) @attribute)

; Definitions
(procedure_declaration name: (name (identifier) @function))
(procedure_declaration name: (name (quoted_identifier) @function))
(trigger_declaration name: (_) @function)
(event_declaration name: (_) @function)

; Variables
(regular_variable_declaration name: (name_or_keyword (name (identifier) @variable.declaration)))
(regular_variable_declaration name: (name_or_keyword (name (quoted_identifier) @variable.declaration)))
(parameter name: (name_or_keyword (name (identifier) @variable.parameter)))
(parameter name: (name_or_keyword (name (quoted_identifier) @variable.parameter)))

; Types
(type_reference (name_or_keyword (name (identifier) @type.builtin)))
(type_reference (name_or_keyword (name (quoted_identifier) @type.builtin)))
(type_reference (qualified_name) @type.builtin)
(label_declaration type: (_) @type.builtin)

; Calls
(postfix_expression
  (primary_expression (name (identifier) @function.call))
  (call_suffix))
(postfix_expression
  (primary_expression (name (quoted_identifier) @function.call))
  (call_suffix))

(member_call_suffix member: (name (identifier) @function.method.call))
(member_call_suffix member: (name (quoted_identifier) @function.method.call))

(scope_call_suffix member: (name (identifier) @function.call))
(scope_call_suffix member: (name (quoted_identifier) @function.call))

; Scope references
(scope_suffix member: (name (identifier) @type.builtin))
(scope_suffix member: (name (quoted_identifier) @type.builtin))
