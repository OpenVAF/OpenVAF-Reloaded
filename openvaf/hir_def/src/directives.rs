//! The compiler directives that supply a default (VAMS-2023 10.2 and 10.3).
//!
//! Neither survives into the syntax tree: the preprocessor consumes the
//! directive and what it set travels with the tokens that follow it, which is
//! what "applies to the text stream following the directive" has to mean once the
//! stream has become a tree. Recovering it is therefore a question about a
//! position, and every caller here has a declaration to ask about.

use basedb::{ErasedAstId, FileId};
use syntax::{DefaultDiscipline, Directives};

use crate::db::HirDefDB;

/// What the directives had set where `item` is declared.
fn directives_at(db: &dyn HirDefDB, root_file: FileId, item: ErasedAstId) -> Directives {
    let pos = db.ast_id_map(root_file).get_syntax(item).range().start();
    db.parse(root_file).directives(pos).clone()
}

/// The discipline that `` `default_discipline `` gives a net declared without
/// one, if one is in force where the net is declared.
pub fn default_discipline(
    db: &dyn HirDefDB,
    root_file: FileId,
    net: ErasedAstId,
) -> Option<DefaultDiscipline> {
    directives_at(db, root_file, net).default_discipline().cloned()
}

/// The default rise and fall time of a transition filter in `module`.
///
/// 10.3 restricts the directive to appearing outside a module definition, so the
/// value in force where a module begins is the value in force for every filter
/// the module contains.
pub fn default_transition(
    db: &dyn HirDefDB,
    root_file: FileId,
    module: ErasedAstId,
) -> Option<f64> {
    directives_at(db, root_file, module).transition.map(|transition| transition.time)
}
