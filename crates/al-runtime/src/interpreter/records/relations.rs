//! The fields a rename reaches. Business Central's Rename "updates the
//! primary key value in all related tables": every field whose
//! `TableRelation` names the renamed table's key gets the new value.
//!
//! The local runtime follows a plain relation (`"Plain"` or
//! `"Plain"."No."`) to a table with a single-field primary key. A
//! conditional or filtered relation, or a relation to part of a composite
//! key, makes a rename of that table need live BC. The test router asks
//! [`RelationIndex::rename_cascades`] the same question, so the two agree.

use std::collections::HashMap;

use al_syntax::IdentifierText;

use super::{
    load_table_meta, object_name_of, parse_field_def, section_body, sections_with_keyword,
};
use crate::interpreter::dispatch::table_code::{field_relation, relation_target};

/// A field whose plain `TableRelation` names another table.
#[derive(Debug, Clone)]
struct PlainRelation {
    /// The table that declares the field.
    table: String,
    field: String,
    /// The related field the relation names, if it names one.
    target_field: Option<String>,
}

/// A field a rename updates: rows of `table` whose `field` holds the old
/// value of the renamed table's `key_field` get the new value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenameCascade {
    pub table: String,
    pub field: String,
    pub key_field: String,
}

/// The `TableRelation` of every workspace table field, by the table it names.
#[derive(Debug, Default)]
pub struct RelationIndex {
    /// Lowercase related table name to the plain relations that name it.
    plain: HashMap<String, Vec<PlainRelation>>,
    /// Lowercase table name to a conditional or filtered relation that may
    /// name it, described for the error.
    unfollowed: HashMap<String, String>,
}

impl RelationIndex {
    /// Read the relations of every table in `source`.
    pub fn build(source: &dyn al_types::ProcedureSource) -> Self {
        let mut index = Self::default();
        for path in source.iter_paths() {
            let Some((text, tree)) = source.get_cached_parse(&path) else {
                continue;
            };
            let bytes = text.as_bytes();
            let mut cursor = tree.root_node().walk();
            for object in tree.root_node().named_children(&mut cursor) {
                let is_table = object.kind() == "object_declaration"
                    && object
                        .child_by_field_name("kind")
                        .is_some_and(|kind| kind.kind() == "kw_table");
                if is_table {
                    index.add_table(object, bytes);
                }
            }
        }
        index
    }

    fn add_table(&mut self, object: tree_sitter::Node<'_>, source: &[u8]) {
        let Some(table) = object_name_of(object, source) else {
            return;
        };
        let Some(fields) = object
            .child_by_field_name("body")
            .and_then(|body| section_body(body, "fields", source))
        else {
            return;
        };
        for section in sections_with_keyword(fields, "field", source) {
            let Ok((_, field, _, _)) = parse_field_def(section, source) else {
                continue;
            };
            let Some(relation) = field_relation(section, source) else {
                continue;
            };
            match relation_target(&relation) {
                Some((target, target_field)) => self
                    .plain
                    .entry(target.to_ascii_lowercase())
                    .or_default()
                    .push(PlainRelation {
                        table: table.clone(),
                        field,
                        target_field,
                    }),
                None => {
                    for name in mentioned_names(&relation) {
                        self.unfollowed.entry(name).or_insert_with(|| {
                            format!(
                                "field {field} of table {table} has TableRelation '{relation}', \
                                 which a local rename cannot follow, so the rename needs live \
                                 Business Central"
                            )
                        });
                    }
                }
            }
        }
    }

    /// The fields a rename of table `table` updates, or why the local
    /// runtime cannot update them and the rename needs live BC.
    pub fn rename_cascades(
        &self,
        source: &dyn al_types::ProcedureSource,
        table: &str,
    ) -> Result<Vec<RenameCascade>, String> {
        let key = table.unquote_identifier().to_ascii_lowercase();
        if let Some(reason) = self.unfollowed.get(&key) {
            return Err(reason.clone());
        }
        let Some(relations) = self.plain.get(&key) else {
            return Ok(Vec::new());
        };
        let meta = load_table_meta(source, table)?;
        let key_fields: Vec<String> = meta
            .pk_fields
            .iter()
            .filter_map(|number| {
                meta.field_by_name
                    .iter()
                    .find(|(_, field)| *field == number)
                    .map(|(name, _)| name.clone())
            })
            .collect();
        let mut cascades = Vec::new();
        for relation in relations {
            let key_field = match &relation.target_field {
                Some(name) => key_fields
                    .iter()
                    .find(|field| field.eq_ignore_ascii_case(name))
                    .cloned(),
                None => key_fields.first().cloned(),
            };
            // A relation to a field outside the key does not change.
            let Some(key_field) = key_field else {
                continue;
            };
            if key_fields.len() != 1 {
                return Err(format!(
                    "field {} of table {} relates to part of the composite primary key of \
                     table {table}, which a local rename cannot follow, so the rename needs live \
                     Business Central",
                    relation.field, relation.table
                ));
            }
            load_table_meta(source, &relation.table)?;
            cascades.push(RenameCascade {
                table: relation.table.clone(),
                field: relation.field.clone(),
                key_field,
            });
        }
        Ok(cascades)
    }
}

/// Every quoted name and bare word in a relation, lowercased. A table the
/// relation names is among them.
fn mentioned_names(relation: &str) -> Vec<String> {
    let mut names = Vec::new();
    for (at, part) in relation.split('"').enumerate() {
        if at % 2 == 1 {
            names.push(part.to_ascii_lowercase());
            continue;
        }
        names.extend(
            part.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .filter(|word| !word.is_empty())
                .map(str::to_ascii_lowercase),
        );
    }
    names
}
