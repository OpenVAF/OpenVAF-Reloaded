mod tree_builder;

use ::preprocessor::{Preprocess, SourceProvider};
use vfs::FileId;

pub(crate) use tree_builder::{Built, KeywordRegions};

use crate::parsing::tree_builder::SyntaxTreeBuilder;

pub(crate) fn parse_text(
    sources: &dyn SourceProvider,
    root_file: FileId,
    Preprocess { ts, sm, .. }: &Preprocess,
) -> Built {
    // tokens without whitespaces/comments
    let parser_tokens: Vec<_> = ts
        .iter()
        .filter_map(|token| {
            if token.kind.is_trivia() {
                return None;
            }
            Some(token.kind)
        })
        .collect();
    let mut builder = SyntaxTreeBuilder::new(sources, root_file, ts, sm);
    for step in parser::parse(&parser_tokens).iter() {
        match step {
            parser::Step::Token { kind } => builder.token(kind),
            parser::Step::Enter { kind } => builder.start_node(kind),
            parser::Step::Exit => builder.finish_node(),
            parser::Step::Error { err } => builder.error(err.clone()),
        }
    }

    builder.finish()
}
