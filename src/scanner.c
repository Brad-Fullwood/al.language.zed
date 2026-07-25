// AUTO-GENERATED - DO NOT EDIT

#include <tree_sitter/parser.h>

#ifdef __wasm__
// Zed's WASI SDK does not provide tree_sitter/alloc.h.
#include <stdlib.h>
#endif

static inline int al_tolower(int c) {
  if (c >= 'A' && c <= 'Z') return c + ('a' - 'A');
  return c;
}

static inline size_t al_strlen(const char *s) {
  size_t n = 0;
  while (s && s[n]) n++;
  return n;
}

static inline void *al_memcpy(void *dst, const void *src, size_t n) {
  char *d = (char *)dst;
  const char *s = (const char *)src;
  for (size_t i = 0; i < n; i++) d[i] = s[i];
  return dst;
}

#ifndef __wasm__
#include <ctype.h>
#include <string.h>
#include <stdlib.h>
#endif

typedef enum {
  KW_ACTION,
  KW_ACTIONREF,
  KW_ANALYSISVIEW,
  KW_ANALYSISVIEWS,
  KW_ARRAY,
  KW_ASSERTERROR,
  KW_AUDITCATEGORY,
  KW_AUTOMATION,
  KW_BEGIN,
  KW_BIGINTEGER,
  KW_BIGTEXT,
  KW_BLOB,
  KW_BOOLEAN,
  KW_BREAK,
  KW_BYTE,
  KW_CASE,
  KW_CHAR,
  KW_CLIENTTYPE,
  KW_CODE,
  KW_CODEUNIT,
  KW_COMPLETIONTRIGGERERRORLEVEL,
  KW_CONNECTIONTYPE,
  KW_CONTINUE,
  KW_CONTROLADDIN,
  KW_COOKIE,
  KW_CUSTOMACTION,
  KW_DATABASE,
  KW_DATACLASSIFICATION,
  KW_DATASCOPE,
  KW_DATATRANSFER,
  KW_DATE,
  KW_DATEFORMULA,
  KW_DATETIME,
  KW_DECIMAL,
  KW_DEFAULTLAYOUT,
  KW_DIALOG,
  KW_DICTIONARY,
  KW_DO,
  KW_DOTNET,
  KW_DOTNETASSEMBLY,
  KW_DOTNETTYPEDECLARATION,
  KW_DOWNTO,
  KW_DURATION,
  KW_ELSE,
  KW_END,
  KW_ENTITLEMENT,
  KW_ENUM,
  KW_ENUMEXTENSION,
  KW_ERRORINFO,
  KW_ERRORTYPE,
  KW_EVENT,
  KW_EXECUTIONCONTEXT,
  KW_EXECUTIONMODE,
  KW_EXIT,
  KW_FIELDCLASS,
  KW_FIELDREF,
  KW_FIELDTYPE,
  KW_FILE,
  KW_FILEUPLOAD,
  KW_FILEUPLOADACTION,
  KW_FILTERPAGEBUILDER,
  KW_FOR,
  KW_FOREACH,
  KW_FUNCTION,
  KW_GUID,
  KW_HTTPCLIENT,
  KW_HTTPCONTENT,
  KW_HTTPHEADERS,
  KW_HTTPREQUESTMESSAGE,
  KW_HTTPREQUESTTYPE,
  KW_HTTPRESPONSEMESSAGE,
  KW_IF,
  KW_IN,
  KW_INDATASET,
  KW_INSTREAM,
  KW_INTEGER,
  KW_INTERFACE,
  KW_INTERNAL,
  KW_ISOLATIONLEVEL,
  KW_JOKER,
  KW_JSONARRAY,
  KW_JSONOBJECT,
  KW_JSONTOKEN,
  KW_JSONVALUE,
  KW_KEYREF,
  KW_LIST,
  KW_LOCAL,
  KW_MEDIA,
  KW_MEDIASET,
  KW_MODULEDEPENDENCYINFO,
  KW_MODULEINFO,
  KW_NONE,
  KW_NOTIFICATION,
  KW_NOTIFICATIONSCOPE,
  KW_OBJECTTYPE,
  KW_OF,
  KW_OPTION,
  KW_OUTSTREAM,
  KW_PAGE,
  KW_PAGEBACKGROUNDTASKERRORLEVEL,
  KW_PAGECUSTOMIZATION,
  KW_PAGEEXTENSION,
  KW_PAGERESULT,
  KW_PAGESTYLE,
  KW_PERMISSIONSET,
  KW_PERMISSIONSETEXTENSION,
  KW_PROCEDURE,
  KW_PROFILE,
  KW_PROFILEEXTENSION,
  KW_PROGRAM,
  KW_PROTECTED,
  KW_QUERY,
  KW_RECORD,
  KW_RECORDID,
  KW_RECORDREF,
  KW_REPEAT,
  KW_REPORT,
  KW_REPORTEXTENSION,
  KW_REPORTFORMAT,
  KW_RUNONCLIENT,
  KW_SECRETTEXT,
  KW_SECURITYFILTER,
  KW_SECURITYFILTERING,
  KW_SECURITYOPERATIONRESULT,
  KW_SESSIONSETTINGS,
  KW_SUPPRESSDISPOSE,
  KW_SYSTEMACTION,
  KW_TABLE,
  KW_TABLECONNECTIONTYPE,
  KW_TABLEEXTENSION,
  KW_TABLEFILTER,
  KW_TEMPORARY,
  KW_TESTACTION,
  KW_TESTFIELD,
  KW_TESTFILTERFIELD,
  KW_TESTHTTPREQUESTMESSAGE,
  KW_TESTHTTPRESPONSEMESSAGE,
  KW_TESTPAGE,
  KW_TESTPERMISSIONS,
  KW_TESTREQUESTPAGE,
  KW_TEXT,
  KW_TEXTBUILDER,
  KW_TEXTCONST,
  KW_TEXTENCODING,
  KW_THEN,
  KW_TIME,
  KW_TO,
  KW_TRANSACTIONMODEL,
  KW_TRANSACTIONTYPE,
  KW_TRIGGER,
  KW_UNTIL,
  KW_VALUE,
  KW_VAR,
  KW_VARIANT,
  KW_VERBOSITY,
  KW_VERSION,
  KW_VIEW,
  KW_VIEWS,
  KW_WEBSERVICEACTIONCONTEXT,
  KW_WEBSERVICEACTIONRESULTCODE,
  KW_WHILE,
  KW_WITH,
  KW_WITHEVENTS,
  KW_XMLATTRIBUTE,
  KW_XMLATTRIBUTECOLLECTION,
  KW_XMLCDATA,
  KW_XMLCOMMENT,
  KW_XMLDECLARATION,
  KW_XMLDOCUMENT,
  KW_XMLDOCUMENTTYPE,
  KW_XMLELEMENT,
  KW_XMLNAMESPACEMANAGER,
  KW_XMLNAMETABLE,
  KW_XMLNODE,
  KW_XMLNODELIST,
  KW_XMLPORT,
  KW_XMLPROCESSINGINSTRUCTION,
  KW_XMLREADOPTIONS,
  KW_XMLTEXT,
  KW_XMLWRITEOPTIONS,
  OP_AND,
  OP_AS,
  OP_DIV,
  OP_IS,
  OP_MOD,
  OP_NOT,
  OP_OR,
  OP_XOR,
  KEYWORD,
  CONTROL_KEYWORD,
  OPERATOR_WORD,
  OBJECT_KEYWORD,
  TYPE_KEYWORD,
  METADATA_KEYWORD,
  PROPERTY_KEYWORD,
  DIRECTIVE,
  INACTIVE_CODE,

} TokenType;

// tree-sitter compiles scanner.c but not the generated lookup separately.
#include "keywords.c"

#define MAX_IF_DEPTH 64
#define MAX_DEFINES 128

typedef struct {
  bool parent_active;
  bool branch_taken;
  bool this_active;
} IfFrame;

typedef struct {
  uint8_t if_depth;
  IfFrame if_stack[MAX_IF_DEPTH];

  bool default_unknown_true;

  char *defines_buf;
  const char *defines[MAX_DEFINES];
  uint16_t defines_len;
} Scanner;

static char *ts_strdup(const char *s) {
  if (!s) return NULL;
  size_t n = al_strlen(s);
  char *out = (char *)malloc(n + 1);
  if (!out) return NULL;
  for (size_t i = 0; i <= n; i++) out[i] = s[i];
  return out;
}

static bool parse_bool_env(const char *s, bool default_value) {
  if (!s || !*s) return default_value;
  char normalized[6] = {0};
  size_t len = al_strlen(s);
  if (len >= sizeof(normalized)) return default_value;
  for (size_t i = 0; i < len; i++) normalized[i] = (char)al_tolower((unsigned char)s[i]);
  if (strcmp(normalized, "1") == 0 || strcmp(normalized, "true") == 0 ||
      strcmp(normalized, "yes") == 0 || strcmp(normalized, "on") == 0) return true;
  if (strcmp(normalized, "0") == 0 || strcmp(normalized, "false") == 0 ||
      strcmp(normalized, "no") == 0 || strcmp(normalized, "off") == 0) return false;
  return default_value;
}

static void scanner_load_defines_from_env(Scanner *scanner) {
#ifdef __wasm__
  return;
#else
  const char *env = getenv("AL_TS_DEFINES");
  if (!env || !*env) return;

  scanner->defines_buf = ts_strdup(env);
  if (!scanner->defines_buf) return;

  char *p = scanner->defines_buf;
  while (*p && scanner->defines_len < MAX_DEFINES) {
    while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r' || *p == ',' || *p == ';') {
      *p = '\0';
      p++;
    }
    if (!*p) break;
    scanner->defines[scanner->defines_len++] = p;
    while (*p && *p != ' ' && *p != '\t' && *p != '\n' && *p != '\r' && *p != ',' && *p != ';') {
      *p = (char)al_tolower((unsigned char)*p);
      p++;
    }
  }
#endif
}

static bool scanner_is_active(const Scanner *scanner) {
  if (!scanner) return true;
  if (scanner->if_depth == 0) return true;
  return scanner->if_stack[scanner->if_depth - 1].this_active;
}

static bool scanner_is_defined(const Scanner *scanner, const char *ident, int len) {
  if (!scanner || !ident || len <= 0) return false;
  for (uint16_t i = 0; i < scanner->defines_len; i++) {
    const char *d = scanner->defines[i];
    if (!d) continue;
    if ((int)al_strlen(d) == len && strncmp(d, ident, (size_t)len) == 0) return true;
  }
  return false;
}

static void expr_skip_ws(const char **p) {
  while (**p == ' ' || **p == '\t') (*p)++;
}

static bool expr_match_word(const char **p, const char *word) {
  const char *s = *p;
  size_t n = al_strlen(word);
  if (strncmp(s, word, n) != 0) return false;
  char next = s[n];
  if ((next >= 'a' && next <= 'z') || (next >= '0' && next <= '9') || next == '_') return false;
  *p = s + n;
  return true;
}

static bool expr_parse_identifier(const char **p, const char **out_start, int *out_len) {
  const char *s = *p;
  char c = *s;
  if (!((c >= 'a' && c <= 'z') || c == '_')) return false;
  const char *start = s;
  s++;
  while ((*s >= 'a' && *s <= 'z') || (*s >= '0' && *s <= '9') || *s == '_') s++;
  *out_start = start;
  *out_len = (int)(s - start);
  *p = s;
  return true;
}

static bool expr_parse_number(const char **p, bool *out_value) {
  const char *s = *p;
  if (!(*s >= '0' && *s <= '9')) return false;
  unsigned long v = 0;
  while (*s >= '0' && *s <= '9') {
    v = v * 10 + (unsigned long)(*s - '0');
    s++;
  }
  *out_value = (v != 0);
  *p = s;
  return true;
}

static bool expr_parse_or(Scanner *scanner, const char **p, bool *out_value);

static bool expr_parse_primary(Scanner *scanner, const char **p, bool *out_value) {
  expr_skip_ws(p);
  if (**p == '(') {
    (*p)++;
    bool inner = false;
    if (!expr_parse_or(scanner, p, &inner)) return false;
    expr_skip_ws(p);
    if (**p == ')') (*p)++;
    *out_value = inner;
    return true;
  }

  if (expr_match_word(p, "defined")) {
    expr_skip_ws(p);
    if (**p == '(') {
      (*p)++;
      expr_skip_ws(p);
      const char *id = NULL;
      int id_len = 0;
      if (expr_parse_identifier(p, &id, &id_len)) {
        expr_skip_ws(p);
        if (**p == ')') (*p)++;
        *out_value = scanner_is_defined(scanner, id, id_len);
        return true;
      }
      return false;
    } else {
      const char *id = NULL;
      int id_len = 0;
      if (expr_parse_identifier(p, &id, &id_len)) {
        *out_value = scanner_is_defined(scanner, id, id_len);
        return true;
      }
      return false;
    }
  }

  bool num_val = false;
  if (expr_parse_number(p, &num_val)) {
    *out_value = num_val;
    return true;
  }

  const char *id = NULL;
  int id_len = 0;
  if (expr_parse_identifier(p, &id, &id_len)) {
    if (id_len == 4 && strncmp(id, "true", 4) == 0) {
      *out_value = true;
      return true;
    }
    if (id_len == 5 && strncmp(id, "false", 5) == 0) {
      *out_value = false;
      return true;
    }
    if (scanner_is_defined(scanner, id, id_len)) {
      *out_value = true;
      return true;
    }
    *out_value = scanner ? scanner->default_unknown_true : true;
    return true;
  }

  return false;
}

static bool expr_parse_unary(Scanner *scanner, const char **p, bool *out_value) {
  expr_skip_ws(p);
  if (**p == '!') {
    (*p)++;
    bool v = false;
    if (!expr_parse_unary(scanner, p, &v)) return false;
    *out_value = !v;
    return true;
  }
  if (expr_match_word(p, "not")) {
    bool v = false;
    if (!expr_parse_unary(scanner, p, &v)) return false;
    *out_value = !v;
    return true;
  }
  return expr_parse_primary(scanner, p, out_value);
}

static bool expr_parse_and(Scanner *scanner, const char **p, bool *out_value) {
  bool left = false;
  if (!expr_parse_unary(scanner, p, &left)) return false;
  for (;;) {
    expr_skip_ws(p);
    if ((**p == '&' && (*p)[1] == '&')) {
      *p += 2;
    } else if (expr_match_word(p, "and")) {
    } else {
      break;
    }
    bool right = false;
    if (!expr_parse_unary(scanner, p, &right)) return false;
    left = left && right;
  }
  *out_value = left;
  return true;
}

static bool expr_parse_or(Scanner *scanner, const char **p, bool *out_value) {
  bool left = false;
  if (!expr_parse_and(scanner, p, &left)) return false;
  for (;;) {
    expr_skip_ws(p);
    if ((**p == '|' && (*p)[1] == '|')) {
      *p += 2;
    } else if (expr_match_word(p, "or")) {
    } else {
      break;
    }
    bool right = false;
    if (!expr_parse_and(scanner, p, &right)) return false;
    left = left || right;
  }
  *out_value = left;
  return true;
}

static bool eval_pp_expr(Scanner *scanner, const char *expr) {
  if (!expr) return true;
  const char *p = expr;
  expr_skip_ws(&p);
  if (!*p) return true;
  bool v = false;
  if (!expr_parse_or(scanner, &p, &v)) {
    // Keep malformed branches visible for parser diagnostics.
    return true;
  }
  return v;
}

static bool scanner_scan_directive(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
  if (!valid_symbols[DIRECTIVE]) return false;

  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    lexer->advance(lexer, true);
  }

  if (lexer->lookahead != '#') return false;

  lexer->advance(lexer, false);

  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    lexer->advance(lexer, true);
  }

  char kw[32];
  int kw_len = 0;
  while (kw_len < (int)sizeof(kw) - 1) {
    int32_t c = lexer->lookahead;
    if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || c == '_') {
      kw[kw_len++] = (char)al_tolower((unsigned char)c);
      lexer->advance(lexer, false);
      continue;
    }
    break;
  }
  kw[kw_len] = '\0';

  while (lexer->lookahead == ' ' || lexer->lookahead == '\t') {
    lexer->advance(lexer, true);
  }

  char expr[512];
  int expr_len = 0;
  while (lexer->lookahead != 0 && lexer->lookahead != '\n') {
    if (expr_len < (int)sizeof(expr) - 1) {
      expr[expr_len++] = (char)al_tolower((unsigned char)lexer->lookahead);
    }
    lexer->advance(lexer, false);
  }
  expr[expr_len] = '\0';

  bool active_outer = scanner_is_active(scanner);

  if (strcmp(kw, "if") == 0) {
    if (scanner && scanner->if_depth < MAX_IF_DEPTH) {
      bool cond = eval_pp_expr(scanner, expr);
      IfFrame frame;
      frame.parent_active = active_outer;
      frame.this_active = active_outer && cond;
      frame.branch_taken = frame.this_active;
      scanner->if_stack[scanner->if_depth++] = frame;
    }
  } else if (strcmp(kw, "elif") == 0) {
    if (scanner && scanner->if_depth > 0) {
      IfFrame *frame = &scanner->if_stack[scanner->if_depth - 1];
      if (!frame->parent_active || frame->branch_taken) {
        frame->this_active = false;
      } else {
        bool cond = eval_pp_expr(scanner, expr);
        frame->this_active = frame->parent_active && cond;
        if (frame->this_active) frame->branch_taken = true;
      }
    }
  } else if (strcmp(kw, "else") == 0) {
    if (scanner && scanner->if_depth > 0) {
      IfFrame *frame = &scanner->if_stack[scanner->if_depth - 1];
      if (!frame->parent_active || frame->branch_taken) {
        frame->this_active = false;
      } else {
        frame->this_active = true;
        frame->branch_taken = true;
      }
    }
  } else if (strcmp(kw, "endif") == 0) {
    if (scanner && scanner->if_depth > 0) {
      scanner->if_depth--;
    }
  }

  lexer->mark_end(lexer);
  lexer->result_symbol = DIRECTIVE;
  return true;
}

static bool scanner_scan_inactive_code(Scanner *scanner, TSLexer *lexer, const bool *valid_symbols) {
  if (!valid_symbols[INACTIVE_CODE]) return false;
  if (scanner_is_active(scanner)) return false;

  // Stop before the next directive line.
  bool consumed = false;
  while (lexer->lookahead != 0 && lexer->lookahead != '\n') {
    lexer->advance(lexer, false);
    consumed = true;
  }
  if (!consumed) return false;

  lexer->mark_end(lexer);
  lexer->result_symbol = INACTIVE_CODE;
  return true;
}

void *tree_sitter_al_external_scanner_create() {
  Scanner *scanner = (Scanner *)calloc(1, sizeof(Scanner));
  if (!scanner) return NULL;
#ifdef __wasm__
  // Unknown symbols are false so `#if not CLEANxx` branches remain visible.
  scanner->default_unknown_true = false;
#else
  scanner->default_unknown_true = parse_bool_env(getenv("AL_TS_UNKNOWN_TRUE"), false);
#endif
  scanner_load_defines_from_env(scanner);
  return scanner;
}

void tree_sitter_al_external_scanner_destroy(void *payload) {
  Scanner *scanner = (Scanner *)payload;
  if (!scanner) return;
  if (scanner->defines_buf) free(scanner->defines_buf);
  free(scanner);
}

unsigned tree_sitter_al_external_scanner_serialize(void *payload, char *buffer) {
  Scanner *scanner = (Scanner *)payload;
  if (!scanner) return 0;

  unsigned size = 0;
  buffer[size++] = (char)scanner->if_depth;
  for (uint8_t i = 0; i < scanner->if_depth && size < TREE_SITTER_SERIALIZATION_BUFFER_SIZE; i++) {
    uint8_t bits = 0;
    if (scanner->if_stack[i].parent_active) bits |= 0x01;
    if (scanner->if_stack[i].branch_taken)  bits |= 0x02;
    if (scanner->if_stack[i].this_active)   bits |= 0x04;
    buffer[size++] = (char)bits;
  }
  return size;
}

void tree_sitter_al_external_scanner_deserialize(void *payload, const char *buffer, unsigned length) {
  Scanner *scanner = (Scanner *)payload;
  if (!scanner) return;

  scanner->if_depth = 0;
  if (!buffer || length == 0) return;

  uint8_t depth = (uint8_t)buffer[0];
  if (depth > MAX_IF_DEPTH) depth = MAX_IF_DEPTH;
  if ((unsigned)(1 + depth) > length) depth = (uint8_t)(length > 1 ? (length - 1) : 0);

  scanner->if_depth = depth;
  for (uint8_t i = 0; i < depth; i++) {
    uint8_t bits = (uint8_t)buffer[1 + i];
    scanner->if_stack[i].parent_active = (bits & 0x01) != 0;
    scanner->if_stack[i].branch_taken  = (bits & 0x02) != 0;
    scanner->if_stack[i].this_active   = (bits & 0x04) != 0;
  }
}

static bool scan_word(TSLexer *lexer, char *out, int out_cap) {
  while (lexer->lookahead == ' ' || lexer->lookahead == '\t' || lexer->lookahead == '\r' || lexer->lookahead == '\n') {
    lexer->advance(lexer, true);
  }

  int32_t c = lexer->lookahead;
  if (!(c == '_' || (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z'))) {
    return false;
  }

  int len = 0;
  while (len < out_cap - 1) {
    int32_t ch = lexer->lookahead;
    if (ch == '_' ||
        (ch >= 'A' && ch <= 'Z') ||
        (ch >= 'a' && ch <= 'z') ||
        (ch >= '0' && ch <= '9')) {
      out[len++] = (char)al_tolower((unsigned char)ch);
      lexer->advance(lexer, false);
      continue;
    }
    break;
  }
  out[len] = '\0';
  lexer->mark_end(lexer);
  return true;
}

bool tree_sitter_al_external_scanner_scan(void *payload, TSLexer *lexer, const bool *valid_symbols) {
  Scanner *scanner = (Scanner *)payload;

  if (!valid_symbols[KW_ACTION] &&
      !valid_symbols[KW_ACTIONREF] &&
      !valid_symbols[KW_ANALYSISVIEW] &&
      !valid_symbols[KW_ANALYSISVIEWS] &&
      !valid_symbols[KW_ARRAY] &&
      !valid_symbols[KW_ASSERTERROR] &&
      !valid_symbols[KW_AUDITCATEGORY] &&
      !valid_symbols[KW_AUTOMATION] &&
      !valid_symbols[KW_BEGIN] &&
      !valid_symbols[KW_BIGINTEGER] &&
      !valid_symbols[KW_BIGTEXT] &&
      !valid_symbols[KW_BLOB] &&
      !valid_symbols[KW_BOOLEAN] &&
      !valid_symbols[KW_BREAK] &&
      !valid_symbols[KW_BYTE] &&
      !valid_symbols[KW_CASE] &&
      !valid_symbols[KW_CHAR] &&
      !valid_symbols[KW_CLIENTTYPE] &&
      !valid_symbols[KW_CODE] &&
      !valid_symbols[KW_CODEUNIT] &&
      !valid_symbols[KW_COMPLETIONTRIGGERERRORLEVEL] &&
      !valid_symbols[KW_CONNECTIONTYPE] &&
      !valid_symbols[KW_CONTINUE] &&
      !valid_symbols[KW_CONTROLADDIN] &&
      !valid_symbols[KW_COOKIE] &&
      !valid_symbols[KW_CUSTOMACTION] &&
      !valid_symbols[KW_DATABASE] &&
      !valid_symbols[KW_DATACLASSIFICATION] &&
      !valid_symbols[KW_DATASCOPE] &&
      !valid_symbols[KW_DATATRANSFER] &&
      !valid_symbols[KW_DATE] &&
      !valid_symbols[KW_DATEFORMULA] &&
      !valid_symbols[KW_DATETIME] &&
      !valid_symbols[KW_DECIMAL] &&
      !valid_symbols[KW_DEFAULTLAYOUT] &&
      !valid_symbols[KW_DIALOG] &&
      !valid_symbols[KW_DICTIONARY] &&
      !valid_symbols[KW_DO] &&
      !valid_symbols[KW_DOTNET] &&
      !valid_symbols[KW_DOTNETASSEMBLY] &&
      !valid_symbols[KW_DOTNETTYPEDECLARATION] &&
      !valid_symbols[KW_DOWNTO] &&
      !valid_symbols[KW_DURATION] &&
      !valid_symbols[KW_ELSE] &&
      !valid_symbols[KW_END] &&
      !valid_symbols[KW_ENTITLEMENT] &&
      !valid_symbols[KW_ENUM] &&
      !valid_symbols[KW_ENUMEXTENSION] &&
      !valid_symbols[KW_ERRORINFO] &&
      !valid_symbols[KW_ERRORTYPE] &&
      !valid_symbols[KW_EVENT] &&
      !valid_symbols[KW_EXECUTIONCONTEXT] &&
      !valid_symbols[KW_EXECUTIONMODE] &&
      !valid_symbols[KW_EXIT] &&
      !valid_symbols[KW_FIELDCLASS] &&
      !valid_symbols[KW_FIELDREF] &&
      !valid_symbols[KW_FIELDTYPE] &&
      !valid_symbols[KW_FILE] &&
      !valid_symbols[KW_FILEUPLOAD] &&
      !valid_symbols[KW_FILEUPLOADACTION] &&
      !valid_symbols[KW_FILTERPAGEBUILDER] &&
      !valid_symbols[KW_FOR] &&
      !valid_symbols[KW_FOREACH] &&
      !valid_symbols[KW_FUNCTION] &&
      !valid_symbols[KW_GUID] &&
      !valid_symbols[KW_HTTPCLIENT] &&
      !valid_symbols[KW_HTTPCONTENT] &&
      !valid_symbols[KW_HTTPHEADERS] &&
      !valid_symbols[KW_HTTPREQUESTMESSAGE] &&
      !valid_symbols[KW_HTTPREQUESTTYPE] &&
      !valid_symbols[KW_HTTPRESPONSEMESSAGE] &&
      !valid_symbols[KW_IF] &&
      !valid_symbols[KW_IN] &&
      !valid_symbols[KW_INDATASET] &&
      !valid_symbols[KW_INSTREAM] &&
      !valid_symbols[KW_INTEGER] &&
      !valid_symbols[KW_INTERFACE] &&
      !valid_symbols[KW_INTERNAL] &&
      !valid_symbols[KW_ISOLATIONLEVEL] &&
      !valid_symbols[KW_JOKER] &&
      !valid_symbols[KW_JSONARRAY] &&
      !valid_symbols[KW_JSONOBJECT] &&
      !valid_symbols[KW_JSONTOKEN] &&
      !valid_symbols[KW_JSONVALUE] &&
      !valid_symbols[KW_KEYREF] &&
      !valid_symbols[KW_LIST] &&
      !valid_symbols[KW_LOCAL] &&
      !valid_symbols[KW_MEDIA] &&
      !valid_symbols[KW_MEDIASET] &&
      !valid_symbols[KW_MODULEDEPENDENCYINFO] &&
      !valid_symbols[KW_MODULEINFO] &&
      !valid_symbols[KW_NONE] &&
      !valid_symbols[KW_NOTIFICATION] &&
      !valid_symbols[KW_NOTIFICATIONSCOPE] &&
      !valid_symbols[KW_OBJECTTYPE] &&
      !valid_symbols[KW_OF] &&
      !valid_symbols[KW_OPTION] &&
      !valid_symbols[KW_OUTSTREAM] &&
      !valid_symbols[KW_PAGE] &&
      !valid_symbols[KW_PAGEBACKGROUNDTASKERRORLEVEL] &&
      !valid_symbols[KW_PAGECUSTOMIZATION] &&
      !valid_symbols[KW_PAGEEXTENSION] &&
      !valid_symbols[KW_PAGERESULT] &&
      !valid_symbols[KW_PAGESTYLE] &&
      !valid_symbols[KW_PERMISSIONSET] &&
      !valid_symbols[KW_PERMISSIONSETEXTENSION] &&
      !valid_symbols[KW_PROCEDURE] &&
      !valid_symbols[KW_PROFILE] &&
      !valid_symbols[KW_PROFILEEXTENSION] &&
      !valid_symbols[KW_PROGRAM] &&
      !valid_symbols[KW_PROTECTED] &&
      !valid_symbols[KW_QUERY] &&
      !valid_symbols[KW_RECORD] &&
      !valid_symbols[KW_RECORDID] &&
      !valid_symbols[KW_RECORDREF] &&
      !valid_symbols[KW_REPEAT] &&
      !valid_symbols[KW_REPORT] &&
      !valid_symbols[KW_REPORTEXTENSION] &&
      !valid_symbols[KW_REPORTFORMAT] &&
      !valid_symbols[KW_RUNONCLIENT] &&
      !valid_symbols[KW_SECRETTEXT] &&
      !valid_symbols[KW_SECURITYFILTER] &&
      !valid_symbols[KW_SECURITYFILTERING] &&
      !valid_symbols[KW_SECURITYOPERATIONRESULT] &&
      !valid_symbols[KW_SESSIONSETTINGS] &&
      !valid_symbols[KW_SUPPRESSDISPOSE] &&
      !valid_symbols[KW_SYSTEMACTION] &&
      !valid_symbols[KW_TABLE] &&
      !valid_symbols[KW_TABLECONNECTIONTYPE] &&
      !valid_symbols[KW_TABLEEXTENSION] &&
      !valid_symbols[KW_TABLEFILTER] &&
      !valid_symbols[KW_TEMPORARY] &&
      !valid_symbols[KW_TESTACTION] &&
      !valid_symbols[KW_TESTFIELD] &&
      !valid_symbols[KW_TESTFILTERFIELD] &&
      !valid_symbols[KW_TESTHTTPREQUESTMESSAGE] &&
      !valid_symbols[KW_TESTHTTPRESPONSEMESSAGE] &&
      !valid_symbols[KW_TESTPAGE] &&
      !valid_symbols[KW_TESTPERMISSIONS] &&
      !valid_symbols[KW_TESTREQUESTPAGE] &&
      !valid_symbols[KW_TEXT] &&
      !valid_symbols[KW_TEXTBUILDER] &&
      !valid_symbols[KW_TEXTCONST] &&
      !valid_symbols[KW_TEXTENCODING] &&
      !valid_symbols[KW_THEN] &&
      !valid_symbols[KW_TIME] &&
      !valid_symbols[KW_TO] &&
      !valid_symbols[KW_TRANSACTIONMODEL] &&
      !valid_symbols[KW_TRANSACTIONTYPE] &&
      !valid_symbols[KW_TRIGGER] &&
      !valid_symbols[KW_UNTIL] &&
      !valid_symbols[KW_VALUE] &&
      !valid_symbols[KW_VAR] &&
      !valid_symbols[KW_VARIANT] &&
      !valid_symbols[KW_VERBOSITY] &&
      !valid_symbols[KW_VERSION] &&
      !valid_symbols[KW_VIEW] &&
      !valid_symbols[KW_VIEWS] &&
      !valid_symbols[KW_WEBSERVICEACTIONCONTEXT] &&
      !valid_symbols[KW_WEBSERVICEACTIONRESULTCODE] &&
      !valid_symbols[KW_WHILE] &&
      !valid_symbols[KW_WITH] &&
      !valid_symbols[KW_WITHEVENTS] &&
      !valid_symbols[KW_XMLATTRIBUTE] &&
      !valid_symbols[KW_XMLATTRIBUTECOLLECTION] &&
      !valid_symbols[KW_XMLCDATA] &&
      !valid_symbols[KW_XMLCOMMENT] &&
      !valid_symbols[KW_XMLDECLARATION] &&
      !valid_symbols[KW_XMLDOCUMENT] &&
      !valid_symbols[KW_XMLDOCUMENTTYPE] &&
      !valid_symbols[KW_XMLELEMENT] &&
      !valid_symbols[KW_XMLNAMESPACEMANAGER] &&
      !valid_symbols[KW_XMLNAMETABLE] &&
      !valid_symbols[KW_XMLNODE] &&
      !valid_symbols[KW_XMLNODELIST] &&
      !valid_symbols[KW_XMLPORT] &&
      !valid_symbols[KW_XMLPROCESSINGINSTRUCTION] &&
      !valid_symbols[KW_XMLREADOPTIONS] &&
      !valid_symbols[KW_XMLTEXT] &&
      !valid_symbols[KW_XMLWRITEOPTIONS] &&
      !valid_symbols[OP_AND] &&
      !valid_symbols[OP_AS] &&
      !valid_symbols[OP_DIV] &&
      !valid_symbols[OP_IS] &&
      !valid_symbols[OP_MOD] &&
      !valid_symbols[OP_NOT] &&
      !valid_symbols[OP_OR] &&
      !valid_symbols[OP_XOR] &&
      !valid_symbols[KEYWORD] &&
      !valid_symbols[CONTROL_KEYWORD] &&
      !valid_symbols[OPERATOR_WORD] &&
      !valid_symbols[OBJECT_KEYWORD] &&
      !valid_symbols[TYPE_KEYWORD] &&
      !valid_symbols[METADATA_KEYWORD] &&
      !valid_symbols[PROPERTY_KEYWORD] &&
      !valid_symbols[DIRECTIVE] &&
      !valid_symbols[INACTIVE_CODE]) {
    return false;
  }


  if (scanner_scan_directive(scanner, lexer, valid_symbols)) {
    return true;
  }
  if (scanner_scan_inactive_code(scanner, lexer, valid_symbols)) {
    return true;
  }


  char word[256];
  if (!scan_word(lexer, word, (int)sizeof(word))) {
    return false;
  }

  // Some words occur in multiple categories, so only return a valid category.
  TokenType specific;
  if (al_lookup_control_kw_token(word, &specific) && valid_symbols[specific]) {
    lexer->result_symbol = specific;
    return true;
  }

  if (al_lookup_operator_word_token(word, &specific) && valid_symbols[specific]) {
    lexer->result_symbol = specific;
    return true;
  }

  if (valid_symbols[CONTROL_KEYWORD] && is_al_control_keyword(word)) {
    lexer->result_symbol = CONTROL_KEYWORD;
    return true;
  }
  if (valid_symbols[OPERATOR_WORD] && is_al_operator_word_keyword(word)) {
    lexer->result_symbol = OPERATOR_WORD;
    return true;
  }
  if (valid_symbols[OBJECT_KEYWORD] && is_al_object_keyword(word)) {
    lexer->result_symbol = OBJECT_KEYWORD;
    return true;
  }
  if (valid_symbols[TYPE_KEYWORD] && is_al_type_keyword(word)) {
    lexer->result_symbol = TYPE_KEYWORD;
    return true;
  }
  if (valid_symbols[METADATA_KEYWORD] && is_al_metadata_keyword(word)) {
    lexer->result_symbol = METADATA_KEYWORD;
    return true;
  }
  if (valid_symbols[PROPERTY_KEYWORD] && is_al_property_keyword(word)) {
    lexer->result_symbol = PROPERTY_KEYWORD;
    return true;
  }
  if (valid_symbols[KEYWORD] && is_al_keyword(word)) {
    lexer->result_symbol = KEYWORD;
    return true;
  }


  return false;
}
