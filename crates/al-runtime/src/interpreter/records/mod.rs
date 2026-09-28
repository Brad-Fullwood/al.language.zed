//! Record and table operations for the AL interpreter.
//!
//! Wires `Value::Record` to the in-memory [`crate::mock::record::MockRecord`]
//! table store so that the common BC `Record` API executes natively in the
//! interpreter: `Init`, field get/set, `Insert`, `Modify`, `Delete`, `Get`,
//! `SetRange`, `SetFilter`, `FindSet`/`FindFirst`/`FindLast`/`Find`/`Next`,
//! `Count`, `IsEmpty`, `Reset`, `SetCurrentKey`, `DeleteAll`.
//!
//! The interpreter is BC-free, so it has **no symbol table** for field numbers
//! or primary keys. Instead, when a record table is first touched, this module
//! looks the table up in the *workspace* (via `DispatchCtx::source`) and parses
//! its `fields { field(N; Name; …) }` and `keys { key(…; F1, F2) }` sections to
//! recover the field-name→number map and primary-key field list. Tables that are
//! not defined in the workspace (e.g. base-app `Customer`) cannot be modelled
//! and return an error.
//!
//! **FlowField / `CalcFormula` evaluation** is implemented for the aggregating
//! formula classes — `Sum`, `Average`, `Min`, `Max`, `Count`, `Exist` and
//! `Lookup`. A field declared `FieldClass = FlowField` with a parseable
//! `CalcFormula` is evaluated against the referenced table's in-memory store,
//! applying the `WHERE` clause (`CONST` / `FIELD` / `FILTER`), both on
//! `CalcFields(<field>)` and on a direct read of the field. `Linked` is not an
//! aggregation and is not modelled (a read of such a field returns an error).
//! Invalid table metadata or FlowField formulas fail explicitly instead of
//! fabricating field IDs, primary keys, or plain-buffer fallbacks.

mod collection_methods;
mod crud;
mod field_access;
mod helpers;
mod relations;
mod store;
mod table_meta;
mod validate;

pub use relations::{RelationIndex, RenameCascade};

pub use store::RecordStore;
pub(crate) use store::{fork_record_for_by_value, record_binding};

pub use table_meta::declares_field_in;
pub(crate) use table_meta::{
    find_table_object, object_name_of, parse_field_def, section_body, sections_with_keyword,
};

pub(crate) use crud::dispatch_record_method;
pub use crud::supports_record_method;

pub(crate) use field_access::{
    declares_field, field_get, implicit_field_get, implicit_field_set, record_field_access,
    try_field_assign, IMPLICIT_RECORD,
};

pub use validate::validate_relation;

pub(crate) use collection_methods::{
    dict_lookup, dispatch_dict_method, dispatch_list_method, dispatch_text_method,
    dispatch_textbuilder_method,
};
pub use collection_methods::{
    supports_dict_method, supports_list_method, supports_text_method, supports_textbuilder_method,
};

pub(crate) use helpers::default_for_structured;
