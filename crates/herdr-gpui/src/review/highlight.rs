//! Syntax colouring for a review's diff, worked out with the diff in the
//! background so drawing only applies prepared spans. Each file's grammar
//! follows its name. Removed lines are read in the order the old file had
//! them and added lines in the new file's, unchanged lines in both; every hunk
//! starts afresh, since the lines between hunks are not in the diff. Tokens
//! are classified, not coloured: the window colours each class from its own
//! theme, so a theme change recolours the diff.
use super::diff::{Diff, Kind};
use std::sync::OnceLock;
use syntect::parsing::{ParseState, Scope, ScopeStack, ScopeStackOp, SyntaxReference, SyntaxSet};

/// What a stretch of code is, as far as its colour goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    Comment,
    String,
    Number,
    Constant,
    Keyword,
    Type,
    Function,
}

/// A coloured stretch of a row's text, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
    pub token: Token,
}

/// The bundled grammars, loaded once, on the first diff that needs them.
fn syntaxes() -> &'static SyntaxSet {
    static SYNTAXES: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAXES.get_or_init(SyntaxSet::load_defaults_newlines)
}

/// Scope prefixes and what they mean, most specific first.
fn classes() -> &'static [(Scope, Token)] {
    static CLASSES: OnceLock<Vec<(Scope, Token)>> = OnceLock::new();
    CLASSES.get_or_init(|| {
        [
            ("comment", Token::Comment),
            ("string", Token::String),
            ("constant.character", Token::String),
            ("constant.numeric", Token::Number),
            ("entity.name.function", Token::Function),
            ("support.function", Token::Function),
            ("variable.function", Token::Function),
            ("entity.name.type", Token::Type),
            ("entity.name.class", Token::Type),
            ("entity.name.struct", Token::Type),
            ("entity.name.enum", Token::Type),
            ("entity.name.tag", Token::Type),
            // Words that introduce a definition read as keywords; other
            // storage types, such as `u32`, as types.
            ("storage.type.function", Token::Keyword),
            ("storage.type.struct", Token::Keyword),
            ("storage.type.enum", Token::Keyword),
            ("storage.type.trait", Token::Keyword),
            ("storage.type.impl", Token::Keyword),
            ("storage.type.class", Token::Keyword),
            ("storage.type.module", Token::Keyword),
            ("storage.type", Token::Type),
            ("support.type", Token::Type),
            ("support.class", Token::Type),
            ("keyword", Token::Keyword),
            ("storage", Token::Keyword),
            ("constant", Token::Constant),
            ("support.constant", Token::Constant),
        ]
        .into_iter()
        .filter_map(|(name, token)| Scope::new(name).ok().map(|scope| (scope, token)))
        .collect()
    })
}

/// The token the innermost classified scope gives, if any.
fn token(stack: &ScopeStack) -> Option<Token> {
    stack.as_slice().iter().rev().find_map(|scope| {
        classes()
            .iter()
            .find(|(prefix, _)| prefix.is_prefix_of(*scope))
            .map(|(_, token)| *token)
    })
}

/// One side of a file being read: its parser and open scopes.
struct Side {
    state: ParseState,
    stack: ScopeStack,
}

impl Side {
    fn new(syntax: &SyntaxReference) -> Self {
        Self {
            state: ParseState::new(syntax),
            stack: ScopeStack::new(),
        }
    }

    /// The spans of `text`, the next line on this side. A line Git or the
    /// grammar cannot follow just goes uncoloured.
    fn line(&mut self, text: &str) -> Vec<Span> {
        let line = format!("{text}\n");
        let Ok(ops) = self.state.parse_line(&line, syntaxes()) else {
            return Vec::new();
        };
        let mut spans: Vec<Span> = Vec::new();
        let mut from = 0;
        let push = |spans: &mut Vec<Span>, start: usize, end: usize, stack: &ScopeStack| {
            let end = end.min(text.len());
            if start >= end {
                return;
            }
            let Some(token) = token(stack) else {
                return;
            };
            match spans.last_mut() {
                Some(last) if last.token == token && last.end == start => last.end = end,
                _ => spans.push(Span { start, end, token }),
            }
        };
        for (at, op) in ops {
            push(&mut spans, from, at, &self.stack);
            if apply(&mut self.stack, &op).is_err() {
                return spans;
            }
            from = at;
        }
        push(&mut spans, from, text.len(), &self.stack);
        spans
    }
}

fn apply(stack: &mut ScopeStack, op: &ScopeStackOp) -> Result<(), syntect::parsing::ScopeError> {
    stack.apply(op).map(|_| ())
}

/// The grammar for a file, by its extension.
fn grammar(name: &str) -> Option<&'static SyntaxReference> {
    let extension = std::path::Path::new(name).extension()?.to_str()?;
    syntaxes().find_syntax_by_extension(extension)
}

/// Colours `diff` in place. Blocking, and seconds for the largest diffs, so
/// it runs on the background executor after the plain diff is shown.
pub(crate) fn colour(diff: &mut Diff) {
    let mut file = usize::MAX;
    let mut syntax = None;
    let mut sides: Option<(Side, Side)> = None;
    for index in 0..diff.rows.len() {
        let row = &diff.rows[index];
        if row.file != file {
            file = row.file;
            syntax = diff.files.get(file).and_then(|name| grammar(name));
            sides = None;
        }
        let Some(syntax) = syntax else {
            continue;
        };
        let (old, new) = sides.get_or_insert_with(|| (Side::new(syntax), Side::new(syntax)));
        let spans = match row.kind {
            // The lines between hunks are not in the diff: start afresh.
            Kind::Hunk => {
                *old = Side::new(syntax);
                *new = Side::new(syntax);
                continue;
            }
            Kind::Added => new.line(&row.text),
            Kind::Removed => old.line(&row.text),
            Kind::Context => {
                old.line(&row.text);
                new.line(&row.text)
            }
            Kind::File | Kind::Meta => continue,
        };
        diff.rows[index].spans = spans;
    }
}

#[cfg(test)]
mod tests;
