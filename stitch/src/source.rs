//! Source registry: resolves a runtime fault's `Span` (a byte range) to a
//! human-readable `name:line:col` + caret. Stitch parses many sources independently
//! (prelude, user program, REPL line/defs, cross-pipe stages, each module), each
//! starting at offset 0, so a span alone can't be rendered — it needs to be paired
//! with the source it indexes. The [`SourceMap`] holds every registered source and a
//! [`SourceId`] names one; closures carry their `SourceId` (set at lowering) so a
//! fault can be resolved back to the right source.

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;

use crate::lexer::Span;

/// An opaque handle naming a registered source. The default (`0`) is the synthetic
/// source — for nodes with no real origin (desugared/invented) — which renders
/// without a location.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SourceId(pub u32);

struct SourceEntry {
    name: String,
    text: String,
}

/// A registry of the sources parsed during a run. Index 0 is reserved for the
/// synthetic source (empty), so `SourceId::default()` renders location-free.
pub struct SourceMap {
    sources: Vec<SourceEntry>,
}

impl Default for SourceMap {
    fn default() -> Self {
        Self::new()
    }
}

impl SourceMap {
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: vec![SourceEntry { name: "<synthetic>".to_string(), text: String::new() }],
        }
    }

    /// Register a source (its display name + text), returning its handle.
    pub fn register(&mut self, name: impl Into<String>, text: impl Into<String>) -> SourceId {
        let id = SourceId(u32::try_from(self.sources.len()).unwrap_or(0));
        self.sources.push(SourceEntry { name: name.into(), text: text.into() });
        id
    }

    /// Render `message` located at `span` within `source` as
    /// `name:line:col: message` + the offending source line + a caret. The
    /// synthetic source (or an unknown id) renders the message alone.
    #[must_use]
    pub fn render(&self, source: SourceId, span: Span, message: &str) -> String {
        match self.sources.get(source.0 as usize) {
            // The synthetic source (id 0, empty text) has no location to show.
            Some(entry) if source.0 != 0 => {
                format!("{}:{}", entry.name, caret_render(&entry.text, span, message))
            }
            _ => message.to_string(),
        }
    }
}

/// Render a `span` against its source `src` as `line:col: message` + the offending
/// line + a caret under the span start. Shared by [`SourceMap::render`] and the
/// parser's `ParseError::render`.
#[must_use]
pub fn caret_render(src: &str, span: Span, message: &str) -> String {
    let offset = span.start.min(src.len());
    let before = &src[..offset];
    let line = before.matches('\n').count() + 1;
    let line_start = before.rfind('\n').map_or(0, |nl| nl + 1);
    let col = src[line_start..offset].chars().count() + 1;
    let line_text = src[line_start..].lines().next().unwrap_or("");
    let caret = " ".repeat(col - 1);
    format!("{line}:{col}: {message}\n{line_text}\n{caret}^")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_a_span_with_source_name_line_and_caret() {
        let mut map = SourceMap::new();
        let id = map.register("hello.st", "main() = 1 / 0");
        // `1 / 0` sits at bytes 9..14; column is 1-based (the `1` is column 10).
        let out = map.render(id, Span { start: 9, end: 14 }, "division by zero");
        assert_eq!(out, "hello.st:1:10: division by zero\nmain() = 1 / 0\n         ^");
    }

    #[test]
    fn the_synthetic_source_renders_the_message_alone() {
        let map = SourceMap::new();
        let out = map.render(SourceId::default(), Span { start: 0, end: 0 }, "boom");
        assert_eq!(out, "boom");
    }
}
