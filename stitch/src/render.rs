//! Shape-dispatched rendering of a Stitch [`Value`] for the human terminal — the
//! Tier-0 renderer (userland design §4). A **homogeneous list of named-field
//! records** becomes a table; everything else falls back to [`Value::display`].
//!
//! The table *model* ([`Table`]: columns + already-rendered cells) is computed
//! once, and a [`TableStyle`] turns it into text — so the look is swappable and a
//! new style is just another `impl`. Pure and host-testable (snapshot the string);
//! no color yet (a Tier-0.5 follow-on, and it only applies on the UART channel).

#[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
use crate::prelude::*;
use crate::value::Value;

/// The style-agnostic model of a table: an optional header row and the rows of
/// already-rendered cells (row-major). A record *list* has a header (the shared
/// field names); a single-record **key/value** table is headerless (each row is
/// `[field, value]`). A [`TableStyle`] turns it into text.
pub struct Table {
    header: Option<Vec<String>>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// The header row (column names), or `None` for a headerless key/value table.
    #[must_use]
    pub fn header(&self) -> Option<&[String]> {
        self.header.as_deref()
    }

    /// The rows, each a vector of cell strings (one per column).
    #[must_use]
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Each column's display width — the widest of its header (if any) and its
    /// cells, counted in **characters** (box-drawing and any non-ASCII are
    /// multi-byte, so byte length would misalign).
    #[must_use]
    pub fn widths(&self) -> Vec<usize> {
        let columns = self
            .header
            .as_ref()
            .map(Vec::len)
            .or_else(|| self.rows.first().map(Vec::len))
            .unwrap_or(0);
        (0..columns)
            .map(|col| {
                let header_width = self.header.as_ref().map_or(0, |h| h[col].chars().count());
                let cell_width =
                    self.rows.iter().map(|row| row[col].chars().count()).max().unwrap_or(0);
                header_width.max(cell_width)
            })
            .collect()
    }
}

/// How a [`Table`] is laid out as text. Every implementation shares the model —
/// only the framing differs — so the renderer's look is swappable.
pub trait TableStyle {
    /// Render `table` to a (multi-line) string, no trailing newline.
    fn render(&self, table: &Table) -> String;
}

/// Unicode box-drawing table: `┌─┬─┐` borders with one space of padding inside
/// each cell.
pub struct BoxStyle;

impl TableStyle for BoxStyle {
    fn render(&self, table: &Table) -> String {
        let widths = table.widths();
        // A horizontal border: each column segment spans `width + 2` (the inside
        // padding), joined by the given tee and capped by the corner chars.
        let border = |left: char, tee: char, right: char| {
            let segments =
                widths.iter().map(|w| "─".repeat(w + 2)).collect::<Vec<_>>();
            format!("{left}{}{right}", segments.join(&tee.to_string()))
        };
        let row = |cells: &[String]| {
            let padded = cells
                .iter()
                .zip(&widths)
                .map(|(cell, width)| format!("{cell:<width$}"))
                .collect::<Vec<_>>();
            format!("│ {} │", padded.join(" │ "))
        };

        let mut out = border('┌', '┬', '┐');
        if let Some(header) = table.header() {
            out.push('\n');
            out.push_str(&row(header));
            out.push('\n');
            out.push_str(&border('├', '┼', '┤'));
        }
        for r in table.rows() {
            out.push('\n');
            out.push_str(&row(r));
        }
        out.push('\n');
        out.push_str(&border('└', '┴', '┘'));
        out
    }
}

/// A homogeneous list of named-field records as a [`Table`], else `None`. Every
/// element must be a `Data` whose fields are **all named**, with the same field
/// names in the same order (the columns). A `Seq` is never tabled (it may be
/// infinite — pipe it through `toList` first).
fn as_table(value: &Value) -> Option<Table> {
    let Value::List(items) = value else {
        return None;
    };
    let (columns, first_row) = record_row(items.first()?)?;
    let mut rows = alloc::vec![first_row];
    for item in &items[1..] {
        let (cols, cells) = record_row(item)?;
        if cols != columns {
            return None; // heterogeneous shapes don't form one table
        }
        rows.push(cells);
    }
    Some(Table { header: Some(columns), rows })
}

/// A record's `(column names, cell strings)`, or `None` if it isn't a `Data` with
/// all fields named.
fn record_row(value: &Value) -> Option<(Vec<String>, Vec<String>)> {
    let Value::Data(data) = value else {
        return None;
    };
    let columns = data.fields.iter().map(|(name, _)| name.clone()).collect::<Option<Vec<_>>>()?;
    let cells = data.fields.iter().map(|(_, v)| v.display()).collect();
    Some((columns, cells))
}

/// A single named-field record as a **headerless** key/value [`Table`] (each row
/// is `[field, value]`), or `None` if it isn't a `Data` with all fields named and
/// at least one field — a nullary variant (like `None`) renders as its name.
fn as_kv(value: &Value) -> Option<Table> {
    let Value::Data(data) = value else {
        return None;
    };
    if data.fields.is_empty() {
        return None;
    }
    let rows = data
        .fields
        .iter()
        .map(|(name, value)| Some(alloc::vec![name.clone()?, value.display()]))
        .collect::<Option<Vec<_>>>()?;
    Some(Table { header: None, rows })
}

/// Render `value` for the terminal against `style`: a homogeneous record *list*
/// becomes a table; a single record becomes a key/value table; anything else falls
/// back to [`Value::display`].
#[must_use]
pub fn render_with(value: &Value, style: &dyn TableStyle) -> String {
    if let Some(table) = as_table(value) {
        return style.render(&table);
    }
    if let Some(kv) = as_kv(value) {
        return style.render(&kv);
    }
    value.display()
}

/// [`render_with`] using the default [`BoxStyle`] — the REPL's result printer.
#[must_use]
pub fn render(value: &Value) -> String {
    render_with(value, &BoxStyle)
}

#[cfg(test)]
mod tests {
    use super::{BoxStyle, Table, TableStyle, render};
    use crate::value::{DataValue, Value};
    #[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
    use crate::prelude::*;

    /// A record `Value` with named fields — a table row.
    fn record(fields: Vec<(&str, Value)>) -> Value {
        Value::Data(Rc::new(DataValue {
            type_name: "R".into(),
            variant: "R".into(),
            fields: fields.into_iter().map(|(n, v)| (Some(n.into()), v)).collect(),
        }))
    }

    #[test]
    fn a_scalar_renders_as_itself() {
        assert_eq!(render(&Value::Int(5)), "5");
        assert_eq!(render(&Value::Str("hi".into())), "hi");
    }

    #[test]
    fn a_non_record_list_falls_back_to_a_flat_display() {
        let list = Value::List(vec![Value::Int(1), Value::Int(2)].into());
        assert_eq!(render(&list), "[1, 2]");
    }

    #[test]
    fn a_list_of_positional_records_is_not_tabled() {
        // Unnamed (positional) fields → no column headers → fall back to display.
        let positional = Value::Data(Rc::new(DataValue {
            type_name: "P".into(),
            variant: "P".into(),
            fields: vec![(None, Value::Int(1))],
        }));
        let list = Value::List(vec![positional].into());
        assert!(!render(&list).contains('│'), "{}", render(&list));
    }

    #[test]
    fn a_heterogeneous_record_list_is_not_tabled() {
        let list = Value::List(
            vec![
                record(vec![("a", Value::Int(1))]),
                record(vec![("b", Value::Int(2))]),
            ]
            .into(),
        );
        assert!(!render(&list).contains('│'), "{}", render(&list));
    }

    #[test]
    fn a_homogeneous_record_list_renders_as_a_box_table() {
        let list = Value::List(
            vec![
                record(vec![("id", Value::Int(1)), ("tag", Value::Str("x".into()))]),
                record(vec![("id", Value::Int(20)), ("tag", Value::Str("yy".into()))]),
            ]
            .into(),
        );
        assert_eq!(
            render(&list),
            "\
┌────┬─────┐
│ id │ tag │
├────┼─────┤
│ 1  │ x   │
│ 20 │ yy  │
└────┴─────┘"
        );
    }

    #[test]
    fn a_single_record_renders_as_a_key_value_table() {
        let cap = record(vec![
            ("handle", Value::Int(0)),
            ("kind", Value::Str("TelemetrySink".into())),
        ]);
        assert_eq!(
            render(&cap),
            "\
┌────────┬───────────────┐
│ handle │ 0             │
│ kind   │ TelemetrySink │
└────────┴───────────────┘"
        );
    }

    #[test]
    fn a_nullary_variant_renders_as_its_name_not_a_table() {
        let none = Value::Data(Rc::new(DataValue {
            type_name: "Maybe".into(),
            variant: "None".into(),
            fields: vec![],
        }));
        assert_eq!(render(&none), "None");
    }

    #[test]
    fn box_style_renders_the_model_directly() {
        let table = Table {
            header: Some(vec!["a".into(), "bb".into()]),
            rows: vec![
                vec!["1".into(), "2".into()],
                vec!["30".into(), "4".into()],
            ],
        };
        assert_eq!(
            BoxStyle.render(&table),
            "\
┌────┬────┐
│ a  │ bb │
├────┼────┤
│ 1  │ 2  │
│ 30 │ 4  │
└────┴────┘"
        );
    }
}
