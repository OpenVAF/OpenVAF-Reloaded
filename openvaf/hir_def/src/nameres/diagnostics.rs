use std::ops::Deref;

use basedb::diagnostics::{Diagnostic, Label, LabelStyle, Report};
use basedb::{AstIdMap, BaseDB, FileId};
use stdx::{impl_display, pretty};
use syntax::name::Name;
use syntax::sourcemap::{FileSpan, SourceMap};
use syntax::{Parse, SourceFile};

use super::{ResolvedPath, ScopeDefItem};
use crate::db::HirDefDB;
use crate::Lookup;

/// Whether a declaration is a paramset (VAMS-2023 6.4) rather than a module, which
/// it shares an item with.
fn is_paramset(db: &dyn HirDefDB, def: &ScopeDefItem) -> bool {
    let ScopeDefItem::ModuleId(module) = *def else { return false };
    let loc = module.lookup(db);
    loc.item_tree(db)[loc.id].paramset.is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathResolveError {
    NotFound { name: Name },
    NotFoundIn { name: Name, scope: Name },
    ExpectedScope { name: Name, found: ScopeDefItem },
    ExpectedItemKind { name: Name, expected: &'static str, found: ResolvedPath },
    ExpectedNatureAttributeIdent { found: Box<[Name]> },
}

impl_display! {
    match PathResolveError{
        PathResolveError::NotFound {name} => "'{}' was not found in the current scope", name;
        PathResolveError::NotFoundIn {name, scope} => "'{}' was not found in '{}'", name, scope;
        PathResolveError::ExpectedScope {name, found} => "expected a scope but found {} '{}'", found.item_kind(), name;
        PathResolveError::ExpectedItemKind{name, expected, found} => "expected {} but found {} '{}'", expected, found, name;
        PathResolveError::ExpectedNatureAttributeIdent{found} => "expected a nature attribute identifier found path {}",  pretty::List::path(found.deref());
    }
}

impl PathResolveError {
    pub fn message(&self) -> String {
        match self {
            PathResolveError::NotFound { .. } | PathResolveError::NotFoundIn { .. } => {
                "not found".to_owned()
            }
            PathResolveError::ExpectedScope { .. }
            | PathResolveError::ExpectedNatureAttributeIdent { .. } => {
                "failed to resolve path".to_owned()
            }
            PathResolveError::ExpectedItemKind { expected, .. } => format!("expected {}", expected),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DefDiagnostic {
    AlreadyDeclared { old: ScopeDefItem, new: ScopeDefItem, name: Name },
}

pub struct DefDiagnosticWrapped<'a> {
    pub db: &'a dyn HirDefDB,
    pub diag: &'a DefDiagnostic,
    pub parse: &'a Parse<SourceFile>,
    pub sm: &'a SourceMap,
    pub ast_id_map: &'a AstIdMap,
}

impl Diagnostic for DefDiagnosticWrapped<'_> {
    fn build_report(&self, _root_file: FileId, _db: &dyn BaseDB) -> Report {
        match self.diag {
            DefDiagnostic::AlreadyDeclared { old, new, name } => {
                let FileSpan { range, file } = self.parse.to_file_span(
                    new.text_range(self.db, self.ast_id_map, self.parse).unwrap(),
                    self.sm,
                );

                let mut labels = vec![Label {
                    style: LabelStyle::Primary,
                    file_id: file,
                    range: range.into(),
                    message: "already declared in this scope".to_owned(),
                }];

                if let Some(def) = old.text_range(self.db, self.ast_id_map, self.parse) {
                    let FileSpan { range, file } = self.parse.to_file_span(def, self.sm);
                    labels.push(Label {
                        style: LabelStyle::Secondary,
                        file_id: file,
                        range: range.into(),
                        message: format!("help '{}' was first declared here", name),
                    })
                }
                // 6.4.2 lets paramsets share a name and leaves the choice between
                // them to elaboration, from the ranges of their parameters. A
                // descriptor is instantiated by name, so by the time one is
                // reached the choice has already been made.
                let notes = if is_paramset(self.db, old) && is_paramset(self.db, new) {
                    vec![
                        "info: VAMS-2023 6.4.2 lets paramsets share a name, and leaves the \
                         choice between them to elaboration, by the ranges of their \
                         parameters; what a simulator instantiates here is a name"
                            .to_owned(),
                        format!(
                            "help: give each paramset a name of its own, or fold them into \
                             one '{name}' whose parameters cover both"
                        ),
                    ]
                } else {
                    Vec::new()
                };

                Report::error()
                    .with_message(format!("'{}' was already declared in this scope", name))
                    .with_labels(labels)
                    .with_notes(notes)
            }
        }
    }
}
