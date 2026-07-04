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

/// The style-agnostic tabular projection of a homogeneous record list: column
/// headers and the already-rendered cell strings (row-major). A [`TableStyle`]
/// turns it into text.
pub struct Table {
    columns: Vec<String>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// The column headers (the records' shared field names).
    #[must_use]
    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The rows, each a vector of cell strings aligned to [`columns`](Self::columns).
    #[must_use]
    pub fn rows(&self) -> &[Vec<String>] {
        &self.rows
    }

    /// Each column's display width — the widest of its header and cells, counted
    /// in **characters** (box-drawing and any non-ASCII are multi-byte, so byte
    /// length would misalign).
    #[must_use]
    pub fn widths(&self) -> Vec<usize> {
        self.columns
            .iter()
            .enumerate()
            .map(|(col, header)| {
                let widest_cell =
                    self.rows.iter().map(|row| row[col].chars().count()).max().unwrap_or(0);
                header.chars().count().max(widest_cell)
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
        out.push('\n');
        out.push_str(&row(table.columns()));
        out.push('\n');
        out.push_str(&border('├', '┼', '┤'));
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
    Some(Table { columns, rows })
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

/// Render `value` for the terminal against `style`: a homogeneous record list
/// becomes a table; anything else falls back to [`Value::display`].
#[must_use]
pub fn render_with(value: &Value, style: &dyn TableStyle) -> String {
    match as_table(value) {
        Some(table) => style.render(&table),
        None => value.display(),
    }
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
    fn box_style_renders_the_model_directly() {
        let table = Table {
            columns: vec!["a".into(), "bb".into()],
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
