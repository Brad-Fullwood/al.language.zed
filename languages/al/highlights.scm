; AUTO-GENERATED - DO NOT EDIT

; Literals
(comment) @comment
(string) @string
(verbatim_string) @string
(integer) @number
(decimal) @number
(date_literal) @number
(time_literal) @number
(datetime_literal) @number

; Preprocessor
(directive) @keyword.directive
(inactive_code) @comment.unused

; Generic captures precede structural overrides because query matches are last-wins.
(identifier) @variable
(quoted_identifier) @variable

; AL is case-insensitive, so TRUE/True/true are all the boolean literal.
((identifier) @constant.builtin
 (#match? @constant.builtin "^([tT][rR][uU][eE]|[fF][aA][lL][sS][eE])$"))

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

(op_not) @keyword.operator

; Type keywords (override control keyword captures)
(kw_action) @type.builtin.al
(kw_actionref) @type.builtin.al
(kw_analysisview) @type.builtin.al
(kw_analysisviews) @type.builtin.al
(kw_array) @type.builtin.al
(kw_auditcategory) @type.builtin.al
(kw_automation) @type.builtin.al
(kw_biginteger) @type.builtin.al
(kw_bigtext) @type.builtin.al
(kw_blob) @type.builtin.al
(kw_boolean) @type.builtin.al
(kw_byte) @type.builtin.al
(kw_char) @type.builtin.al
(kw_clienttype) @type.builtin.al
(kw_code) @type.builtin.al
(kw_codeunit) @type.builtin.al
(kw_completiontriggererrorlevel) @type.builtin.al
(kw_connectiontype) @type.builtin.al
(kw_cookie) @type.builtin.al
(kw_customaction) @type.builtin.al
(kw_database) @type.builtin.al
(kw_dataclassification) @type.builtin.al
(kw_datascope) @type.builtin.al
(kw_datatransfer) @type.builtin.al
(kw_date) @type.builtin.al
(kw_dateformula) @type.builtin.al
(kw_datetime) @type.builtin.al
(kw_decimal) @type.builtin.al
(kw_defaultlayout) @type.builtin.al
(kw_dialog) @type.builtin.al
(kw_dictionary) @type.builtin.al
(kw_dotnet) @type.builtin.al
(kw_dotnetassembly) @type.builtin.al
(kw_dotnettypedeclaration) @type.builtin.al
(kw_duration) @type.builtin.al
(kw_enum) @type.builtin.al
(kw_errorinfo) @type.builtin.al
(kw_errortype) @type.builtin.al
(kw_executioncontext) @type.builtin.al
(kw_executionmode) @type.builtin.al
(kw_fieldclass) @type.builtin.al
(kw_fieldref) @type.builtin.al
(kw_fieldtype) @type.builtin.al
(kw_file) @type.builtin.al
(kw_fileupload) @type.builtin.al
(kw_fileuploadaction) @type.builtin.al
(kw_filterpagebuilder) @type.builtin.al
(kw_guid) @type.builtin.al
(kw_httpclient) @type.builtin.al
(kw_httpcontent) @type.builtin.al
(kw_httpheaders) @type.builtin.al
(kw_httprequestmessage) @type.builtin.al
(kw_httprequesttype) @type.builtin.al
(kw_httpresponsemessage) @type.builtin.al
(kw_instream) @type.builtin.al
(kw_integer) @type.builtin.al
(kw_interface) @type.builtin.al
(kw_isolationlevel) @type.builtin.al
(kw_joker) @type.builtin.al
(kw_jsonarray) @type.builtin.al
(kw_jsonobject) @type.builtin.al
(kw_jsontoken) @type.builtin.al
(kw_jsonvalue) @type.builtin.al
(kw_keyref) @type.builtin.al
(kw_list) @type.builtin.al
(kw_media) @type.builtin.al
(kw_mediaset) @type.builtin.al
(kw_moduledependencyinfo) @type.builtin.al
(kw_moduleinfo) @type.builtin.al
(kw_none) @type.builtin.al
(kw_notification) @type.builtin.al
(kw_notificationscope) @type.builtin.al
(kw_objecttype) @type.builtin.al
(kw_option) @type.builtin.al
(kw_outstream) @type.builtin.al
(kw_page) @type.builtin.al
(kw_pagebackgroundtaskerrorlevel) @type.builtin.al
(kw_pageresult) @type.builtin.al
(kw_pagestyle) @type.builtin.al
(kw_query) @type.builtin.al
(kw_record) @type.builtin.al
(kw_recordid) @type.builtin.al
(kw_recordref) @type.builtin.al
(kw_report) @type.builtin.al
(kw_reportformat) @type.builtin.al
(kw_secrettext) @type.builtin.al
(kw_securityfilter) @type.builtin.al
(kw_securityfiltering) @type.builtin.al
(kw_securityoperationresult) @type.builtin.al
(kw_sessionsettings) @type.builtin.al
(kw_systemaction) @type.builtin.al
(kw_table) @type.builtin.al
(kw_tableconnectiontype) @type.builtin.al
(kw_tablefilter) @type.builtin.al
(kw_testaction) @type.builtin.al
(kw_testfield) @type.builtin.al
(kw_testfilterfield) @type.builtin.al
(kw_testhttprequestmessage) @type.builtin.al
(kw_testhttpresponsemessage) @type.builtin.al
(kw_testpage) @type.builtin.al
(kw_testpermissions) @type.builtin.al
(kw_testrequestpage) @type.builtin.al
(kw_text) @type.builtin.al
(kw_textbuilder) @type.builtin.al
(kw_textconst) @type.builtin.al
(kw_textencoding) @type.builtin.al
(kw_time) @type.builtin.al
(kw_transactionmodel) @type.builtin.al
(kw_transactiontype) @type.builtin.al
(kw_variant) @type.builtin.al
(kw_verbosity) @type.builtin.al
(kw_version) @type.builtin.al
(kw_view) @type.builtin.al
(kw_views) @type.builtin.al
(kw_webserviceactioncontext) @type.builtin.al
(kw_webserviceactionresultcode) @type.builtin.al
(kw_xmlattribute) @type.builtin.al
(kw_xmlattributecollection) @type.builtin.al
(kw_xmlcdata) @type.builtin.al
(kw_xmlcomment) @type.builtin.al
(kw_xmldeclaration) @type.builtin.al
(kw_xmldocument) @type.builtin.al
(kw_xmldocumenttype) @type.builtin.al
(kw_xmlelement) @type.builtin.al
(kw_xmlnamespacemanager) @type.builtin.al
(kw_xmlnametable) @type.builtin.al
(kw_xmlnode) @type.builtin.al
(kw_xmlnodelist) @type.builtin.al
(kw_xmlport) @type.builtin.al
(kw_xmlprocessinginstruction) @type.builtin.al
(kw_xmlreadoptions) @type.builtin.al
(kw_xmltext) @type.builtin.al
(kw_xmlwriteoptions) @type.builtin.al


(operator_word) @keyword.operator
(object_keyword) @keyword
; A builtin type used in code (`Database`, `ObjectType`) is a builtin type
; keyword, which BC themes draw in the keyword color.
(type_keyword) @type.builtin.al
(metadata_keyword) @keyword
(property_keyword) @keyword
(keyword) @keyword
(control_keyword) @keyword.control
(kw_keys) @keyword
(kw_key) @keyword
(movement_directive) @keyword

; Punctuation and operators
(operator) @operator
; A sign is a leaf. The `not` form wraps op_not, which has its own capture.
((unary_operator) @operator
 (#match? @operator "^[-+!]$"))
(semicolon) @punctuation
(comma) @punctuation
["." ":" "::"] @punctuation
"=" @operator
["(" ")" "[" "]" "{" "}"] @punctuation.bracket

; A case label with a leading minus is one token: `-1`, `-Limit`,
; `-Level::Gold.AsInteger()`. A number reads as a number, anything else as a
; variable.
(signed_case_label) @variable
((signed_case_label) @number
 (#match? @number "^-[ \t\r\n]*[0-9]"))

; The kind of an object declaration is a keyword, as Microsoft's BC themes draw
; it. `codeunit`, `table` and the other kinds that also name a variable type are
; type.builtin above, so this pattern comes later.
(object_declaration kind: _ @keyword)

; Namespace names, in `namespace` and `using`, are drawn as types, as Microsoft's
; BC themes color entity.name.namespace.
(namespace_or_using_declaration name: (name [(identifier) (quoted_identifier)] @type))
(namespace_or_using_declaration
  name: (qualified_name (name [(identifier) (quoted_identifier)] @type)))

; Object declarations. The leaf is captured rather than the name_or_keyword
; wrapper so the generic (identifier)/(quoted_identifier) captures above do not
; win inside the wrapper's span. The name is a type, as Microsoft's AL
; extension tags it (`class`).
(object_declaration name: (name_or_keyword (name (quoted_identifier) @type)))
(object_declaration name: (name_or_keyword (name (identifier) @type)))
(object_declaration
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (keyword)
  ] @type))

; Properties. Each capture is on a leaf: a capture on the name wrapper loses
; to the identifier capture inside it.
(property_assignment name: [(property_keyword) (metadata_keyword) (keyword)] @property)
(property_assignment name: (name [(identifier) (quoted_identifier)] @property))

; A word in a property's value is one of the property's values, an enum
; member (`DataClassification = ToBeClassified`), except `true` and `false`.
(property_assignment
  name: (_)
  (name (identifier) @constant.enum.al)
  (#not-match? @constant.enum.al "^([tT][rR][uU][eE]|[fF][aA][lL][sS][eE])$"))
(property_assignment
  name: (_)
  (name (quoted_identifier) @type.builtin))
; The value of a property that names an object is that object.
(property_assignment
  name: (_) @_property
  (name (identifier) @type.builtin)
  (#match? @_property "^(?i)(SourceTable|TableRelation|LookupPageId|DrillDownPageId|CardPageId|RunObject|LinkedObject|DataItemTable|SourceTableView|PageId|TableNo|DefaultRenderingLayout|DefaultLayout)$"))
; In `Permissions`, a name is an object (`tabledata Item`), and a name made
; only of permission letters (`r`, `rimd`, `X`) is the permissions, drawn as
; plain text.
(property_assignment
  name: (_) @_property
  (name (identifier) @type.builtin)
  (#match? @_property "^(?i)Permissions$"))
(property_assignment
  name: (_) @_property
  (name (identifier) @permission.al)
  (#match? @_property "^(?i)Permissions$")
  (#match? @permission.al "^(?i)[rimdx]+$"))

; Attributes. Microsoft's AL extension scopes an attribute name
; entity.other.attribute, which BC themes draw in the foreground.
(attribute name: (identifier) @attribute.al)

; Names of page controls and actions, report and query columns and data items,
; page views, report layouts and xmlport elements: the first name in
; `keyword(Name ...)`. Microsoft scopes them entity.name.class, the type
; color. A table field starts with its number, so `field(1; Name; Type)` does
; not match.
(object_section
  keyword: (_) @_section
  (parenthesized_block . [(identifier) (quoted_identifier) (control_keyword)] @type)
  (#match? @_section "^(?i)(field|group|part|repeater|cuegroup|fixed|grid|usercontrol|systempart|label|addfirst|addlast|addafter|addbefore|movefirst|movelast|moveafter|movebefore|modify|action|actionref|separator|customaction|fileuploadaction|systemaction|column|dataitem|filter|view|layout|textelement|fieldelement|fieldattribute|tableelement)$"))
; The table of a data item and the page of a part name an object.
(object_section
  keyword: (_) @_section
  (parenthesized_block
    (semicolon) @_separator
    .
    [(identifier) (quoted_identifier)] @type.builtin)
  (#match? @_section "^(?i)(dataitem|part)$"))
; A report's data items, under `dataset`, are variables in report code, and
; Microsoft colors their names as variables. Query data items stay types.
; Three levels of nesting are matched.
(object_section
  keyword: (_) @_dataset
  body: (object_body
    (object_section
      keyword: (_) @_dataitem
      (parenthesized_block . [(identifier) (quoted_identifier)] @variable)))
  (#match? @_dataset "^(?i)dataset$")
  (#match? @_dataitem "^(?i)dataitem$"))
(object_section
  keyword: (_) @_dataset
  body: (object_body
    (object_section
      keyword: (_) @_outer
      body: (object_body
        (object_section
          keyword: (_) @_dataitem
          (parenthesized_block . [(identifier) (quoted_identifier)] @variable)))))
  (#match? @_dataset "^(?i)dataset$")
  (#match? @_outer "^(?i)dataitem$")
  (#match? @_dataitem "^(?i)dataitem$"))
(object_section
  keyword: (_) @_dataset
  body: (object_body
    (object_section
      keyword: (_) @_outer
      body: (object_body
        (object_section
          keyword: (_) @_middle
          body: (object_body
            (object_section
              keyword: (_) @_dataitem
              (parenthesized_block . [(identifier) (quoted_identifier)] @variable)))))))
  (#match? @_dataset "^(?i)dataset$")
  (#match? @_outer "^(?i)dataitem$")
  (#match? @_middle "^(?i)dataitem$")
  (#match? @_dataitem "^(?i)dataitem$"))
; The object an extension extends, or an interface a codeunit implements.
(implements_clause (name [(identifier) (quoted_identifier)] @type))

; Definitions
(procedure_declaration name: (name (identifier) @function))
(procedure_declaration name: (name (quoted_identifier) @function))
(trigger_declaration name: (name_or_keyword (name [(identifier) (quoted_identifier)] @function)))
(trigger_declaration
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @function))
(event_declaration name: (name_or_keyword (name [(identifier) (quoted_identifier)] @function)))
(event_declaration
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @function))

; Variables
(regular_variable_declaration name: (name_or_keyword (name (identifier) @variable.declaration)))
(regular_variable_declaration name: (name_or_keyword (name (quoted_identifier) @variable.declaration)))
(parameter name: (name_or_keyword (name (identifier) @variable.parameter)))
(parameter name: (name_or_keyword (name (quoted_identifier) @variable.parameter)))
(label_declaration name: (name_or_keyword (name [(identifier) (quoted_identifier)] @variable.declaration)))

; A variable, label or parameter may be named after a keyword (Page, Value,
; Code). Where it is declared, and where it is used as a plain value, it keeps
; the variable capture. Before a member or scope suffix the word may be the
; object itself (Page.RunModal), so there it keeps the keyword capture.
(regular_variable_declaration
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @variable.declaration))
(label_declaration
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @variable.declaration))
(parameter
  name: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @variable.parameter))
(for_statement
  iterator: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @variable))
(foreach_statement
  iterator: (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (kw_function)
    (keyword)
  ] @variable))
(postfix_expression
  (primary_expression [(object_keyword) (type_keyword)] @variable) .)
(postfix_expression
  (primary_expression [(object_keyword) (type_keyword)] @variable)
  .
  (index_suffix))

; Types
(type_reference (name_or_keyword (name (identifier) @type.builtin)))
(type_reference (name_or_keyword (name (quoted_identifier) @type.builtin)))
(type_reference (qualified_name (name [(identifier) (quoted_identifier)] @type.builtin)))
(label_declaration type: (_) @type.builtin.al)

; Element types after `of`: `array[3] of Enum "Level"`, `List of [Text]`,
; `Dictionary of [Code[20], List of [Integer]]`. The words inside the brackets
; are plain tokens in the tree, so each nesting level has its own pattern.
(of_clause (name_or_keyword (name [(identifier) (quoted_identifier)] @type.builtin)))
(of_clause
  (name_or_keyword [
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (keyword)
  ] @type.builtin))
(of_clause
  (bracketed_block [
    (control_keyword)
    (type_keyword)
    (object_keyword)
    (metadata_keyword)
    (property_keyword)
    (keyword)
    (identifier)
    (quoted_identifier)
  ] @type.builtin))
(of_clause
  (bracketed_block
    (bracketed_block [
      (control_keyword)
      (type_keyword)
      (object_keyword)
      (metadata_keyword)
      (property_keyword)
      (keyword)
      (identifier)
      (quoted_identifier)
    ] @type.builtin)))
(of_clause
  (bracketed_block
    (bracketed_block
      (bracketed_block [
        (control_keyword)
        (type_keyword)
        (object_keyword)
        (metadata_keyword)
        (property_keyword)
        (keyword)
        (identifier)
        (quoted_identifier)
      ] @type.builtin))))
; The `of` of a nested List or Dictionary type is a control keyword token.
((control_keyword) @keyword.control
 (#match? @keyword.control "^[oO][fF]$"))

; Calls
(postfix_expression
  (primary_expression (name (identifier) @function.call))
  (call_suffix))
(postfix_expression
  (primary_expression (name (quoted_identifier) @function.call))
  (call_suffix))
; A method named after a keyword, such as TestField in table code.
(postfix_expression
  (primary_expression [(object_keyword) (type_keyword)] @function.call)
  .
  (call_suffix))

(member_call_suffix member: (name (identifier) @function.method.call))
(member_call_suffix member: (name (quoted_identifier) @function.method.call))

(scope_call_suffix member: (name (identifier) @function.call))
(scope_call_suffix member: (name (quoted_identifier) @function.call))

; Built-in calls. Microsoft's AL extension scopes a built-in function
; support.function, which BC themes draw in the foreground, and the language
; server sends these as builtinFunction tokens. A global built-in comes from
; data/builtin_functions.json. A member is built in when its name is a Record
; method (Microsoft's TableClass metadata, as in al-syntax's
; record_methods.json) or when it is called on an object kind or a system
; scope (`Report.Run`, `Session.SessionId`). A query cannot see the receiver's
; type, so a user procedure named like a Record method also matches.
(postfix_expression
  (primary_expression (name (identifier) @function.builtin.al))
  (call_suffix)
  (#match? @function.builtin.al "^(?i)(Abs|ApplicationPath|ArrayLen|CalcDate|Clear|ClearAll|ClearLastError|Commit|CompanyName|CompressArray|Confirm|ConvertStr|CopyArray|CopyStr|CreateDateTime|CreateGuid|CurrentDateTime|DMY2Date|DT2Date|DT2Time|Date2DMY|DelChr|DelStr|Dialog|Download|DownloadFromStream|Error|Evaluate|FieldCaption|FieldNo|Format|FromInteger|GetDotNetType|GetLastErrorCode|GetLastErrorText|GlobalLanguage|GuiAllowed|Hyperlink|IncStr|InsStr|IsNull|IsNullGuid|LowerCase|Maximum|Message|Minimum|Names|NormalDate|NullGuid|Ordinals|PadStr|Power|Random|Randomize|Rollback|Round|Run|RunModal|SelectStr|Sleep|SortArray|Sqrt|StrLen|StrMenu|StrPos|StrSubstNo|TableCaption|TenantId|Time|Today|TypeHelper|Upload|UploadIntoStream|UpperCase|UserId|Variant2Date|Variant2Time|View|ViewFromStream|WindowsLanguage|WorkDate)$"))
(member_call_suffix
  member: (name (identifier) @function.builtin.al)
  (#match? @function.builtin.al "^(?i)(AddLink|AddLoadFields|AreFieldsLoaded|Ascending|CalcFields|CalcSums|ChangeCompany|ClearMarks|Consistent|Copy|CopyFilter|CopyFilters|CopyLinks|Count|CountApprox|CurrentCompany|CurrentKey|Delete|DeleteAll|DeleteLink|DeleteLinks|FieldActive|FieldCaption|FieldError|FieldName|FieldNo|FilterGroup|Find|FindFirst|FindLast|FindSet|FullyQualifiedName|Get|GetAscending|GetBySystemId|GetFilter|GetFilters|GetPosition|GetRangeMax|GetRangeMin|GetView|HasFilter|HasLinks|Init|Insert|IsEmpty|IsTemporary|LoadFields|LockTable|Mark|MarkedOnly|Modify|ModifyAll|Next|ReadConsistency|ReadIsolation|ReadPermission|RecordId|RecordLevelLocking|Relation|Rename|Reset|SecurityFiltering|SetAscending|SetAutoCalcFields|SetBaseLoadFields|SetCurrentKey|SetFilter|SetLoadFields|SetPermissionFilter|SetPosition|SetRange|SetRecFilter|SetView|TableCaption|TableName|TestField|TransferFields|Truncate|Validate|WritePermission)$"))
(postfix_expression
  (primary_expression) @_receiver
  (member_call_suffix member: (name (identifier) @function.builtin.al))
  (#match? @_receiver "^(?i)(Codeunit|Database|Page|Query|Report|XmlPort|Session|System|CurrentSession|CompanyProperty|NavApp|NumberSequence|TaskScheduler|Debugger|IsolatedStorage|ProductName)$"))

; Scope references. A name after `::` is an enum or option member
; (`Status::Released`, `"Document Type"::"Purchase Receipt"`), unless what
; precedes `::` is an object kind, `Database`, `Enum` or `Interface`, which
; names an object (`Codeunit::"Sales-Post"`).
(scope_suffix member: (name [(identifier) (quoted_identifier)] @constant.enum.al))
(postfix_expression
  (primary_expression (object_keyword)) @_scope
  .
  (scope_suffix member: (name [(identifier) (quoted_identifier)] @type.builtin)))
(postfix_expression
  (primary_expression (type_keyword) @_scope)
  .
  (scope_suffix member: (name [(identifier) (quoted_identifier)] @type.builtin))
  (#match? @_scope "^(?i)(Database|Enum|Interface)$"))
