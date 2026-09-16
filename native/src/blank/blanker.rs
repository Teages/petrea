use oxc_parser::{Kind, Token};
use oxc_span::Span;

use super::blank_string::BlankString;
use super::trivia::TokenIndex;

/// A TypeScript-only construct with runtime semantics; positions are byte offsets.
#[derive(Debug, Clone)]
pub struct UnsupportedSyntax {
    pub node_type: &'static str,
    pub start: u32,
    pub end: u32,
}

pub struct Blanker<'a> {
    pub output: BlankString,
    pub tokens: TokenIndex<'a>,
    /// True while the previously emitted JS did not end with a `;`.
    pub semicolon_needed: bool,
    pub reports: Vec<UnsupportedSyntax>,
}

impl<'a> Blanker<'a> {
    pub fn new(src: &'a str, tokens: &'a [Token]) -> Self {
        Self {
            output: BlankString::default(),
            tokens: TokenIndex::new(src, tokens),
            semicolon_needed: false,
            reports: Vec::new(),
        }
    }

    /// Report an unsupported construct; its source stays in the output.
    pub fn report(&mut self, node_type: &'static str, span: Span) {
        self.reports.push(UnsupportedSyntax {
            node_type,
            start: span.start,
            end: span.end,
        });
    }

    pub fn blank_range(&mut self, start: u32, end: u32) {
        self.output.blank(start, end);
    }

    pub fn blank_span(&mut self, span: Span) {
        self.output.blank(span.start, span.end);
    }

    /// Blank a statement-like node being fully erased, emitting a leading `;`
    /// when the previous emitted JS lacks one, so a following statement cannot
    /// merge into it (ASI protection).
    pub fn blank_statement(&mut self, span: Span) {
        if self.semicolon_needed {
            self.output.blank_but_start_with_semi(span.start, span.end);
        } else {
            self.output.blank(span.start, span.end);
        }
    }

    /// oxc type-annotation spans include the leading `:`.
    pub fn blank_type_annotation(&mut self, span: Span) {
        self.output.blank(span.start, span.end);
    }

    pub fn blank_exact_and_optional_trailing_comma(&mut self, span: Span) {
        let end = match self.tokens.token_from(span.end) {
            Some(token) if token.kind() == Kind::Comma => token.span().end,
            _ => span.end,
        };
        self.output.blank(span.start, end);
    }

    pub fn ends_with_semicolon(&self, span: Span) -> bool {
        self.tokens.ends_with_semicolon(span.start, span.end)
    }

    pub fn blank_marker_char(&mut self, anchor: u32, marker: Kind) {
        if let Some(span) = self.tokens.marker_before(anchor, marker) {
            self.output.blank(span.start, span.end);
        }
    }
}
