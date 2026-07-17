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
use crate::value::{DataValue, Value};
use unicode_width::UnicodeWidthStr;

/// The number of terminal cells `text` occupies — what a column must be padded
/// to so borders line up. `char` count won't do: an emoji is one `char` but two
/// cells, and a VS16-selected emoji (`✏️` = `U+270F` + `U+FE0F`) is two `char`s
/// for the same two cells. `unicode-width` (0.2+) scores all of these correctly,
/// including the VS16 promotion of an otherwise one-cell symbol, so this is a
/// thin wrapper that names the intent at the call sites.
fn display_width(text: &str) -> usize {
    text.width()
}

/// A column's horizontal alignment. Decided from the *value type* (numbers
/// right-align, everything else left) — read off the value, never by re-parsing
/// the rendered cell string.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    /// Left-justified — the default (text, records, anything non-numeric).
    Left,
    /// Right-justified — a column whose every cell is numeric.
    Right,
}

/// The style-agnostic model of a table: an optional header row, the per-column
/// alignments, and the rows of already-rendered cells (row-major). A record
/// *list* has a header (the shared field names); a single-record **key/value**
/// table is headerless (each row is `[field, value]`). A [`TableStyle`] turns it
/// into text.
pub struct Table {
    header: Option<Vec<String>>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
    /// Per-row provenance (one per entry in `rows`): `true` when that row came
    /// from a kernel-built (`native`) record. A colorizer keys on this so only
    /// genuine kernel data (e.g. `hold`'s rights) is colored — never a user
    /// record that merely looks the same. `false` for every row of a
    /// user-constructed table.
    row_native: Vec<bool>,
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

    /// Each column's horizontal alignment (one per column).
    #[must_use]
    pub fn aligns(&self) -> &[Align] {
        &self.aligns
    }

    /// Per-row provenance (one per row): whether that row came from a
    /// kernel-built record. A colorizer paints only these rows.
    #[must_use]
    pub fn row_native(&self) -> &[bool] {
        &self.row_native
    }

    /// Each column's display width — the widest of its header (if any) and its
    /// cells, counted in **terminal cells** (`display_width`): an emoji is one
    /// `char` but two cells, so neither byte nor `char` length would align.
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
                let header_width = self.header.as_ref().map_or(0, |h| display_width(&h[col]));
                let cell_width =
                    self.rows.iter().map(|row| display_width(&row[col])).max().unwrap_or(0);
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
/// each cell. Carries a `colorize` hook — the identity for [`plain`](Self::plain),
/// an ANSI wrapper (e.g. [`colorize_rights`](crate::platform::colorize_rights))
/// for a colored channel. The hook is given each data row's **provenance** (was
/// it kernel-built?) alongside the content, so a colorizer can paint only genuine
/// kernel data — coloring keys on *where the row came from*, not on which glyphs
/// appear or what the column is named, so a user record can never spoof it.
/// Colorizing happens *after* width measurement, so the escape bytes (which are
/// printable ASCII, not zero-width to a measurer) never shift the layout.
pub struct BoxStyle {
    colorize: fn(bool, &str) -> String,
}

fn uncolored(_native: bool, cell: &str) -> String {
    cell.to_string()
}

impl BoxStyle {
    /// A box table with no color — cell content is drawn verbatim.
    #[must_use]
    pub fn plain() -> Self {
        BoxStyle { colorize: uncolored }
    }

    /// A box table whose cell content is passed through `colorize` (given each
    /// row's provenance + the content) after layout is measured.
    #[must_use]
    pub fn with_colorizer(colorize: fn(bool, &str) -> String) -> Self {
        BoxStyle { colorize }
    }
}

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
        let aligns = table.aligns();
        // Pad by *display width*, not `char` count: `{cell:<width$}` counts chars,
        // so a two-cell emoji would be under-padded and shear the border. `native`
        // is the row's provenance — the colorizer paints only kernel-built rows.
        let pad = |colorize: fn(bool, &str) -> String,
                   native: bool,
                   cell: &str,
                   width: usize,
                   align: Align| {
            let fill = " ".repeat(width.saturating_sub(display_width(cell)));
            let shown = colorize(native, cell);
            match align {
                Align::Left => format!("{shown}{fill}"),
                Align::Right => format!("{fill}{shown}"),
            }
        };
        // The header row is labels, not data — never colorized. Each data row runs
        // the installed colorizer with its own provenance.
        let row = |cells: &[String], colorize: fn(bool, &str) -> String, native: bool| {
            let padded = cells
                .iter()
                .zip(&widths)
                .zip(aligns)
                .map(|((cell, width), align)| pad(colorize, native, cell, *width, *align))
                .collect::<Vec<_>>();
            format!("│ {} │", padded.join(" │ "))
        };

        let mut out = border('┌', '┬', '┐');
        if let Some(header) = table.header() {
            out.push('\n');
            out.push_str(&row(header, uncolored, false));
            out.push('\n');
            out.push_str(&border('├', '┼', '┤'));
        }
        for (r, native) in table.rows().iter().zip(table.row_native()) {
            out.push('\n');
            out.push_str(&row(r, self.colorize, *native));
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
    let first = items.first()?;
    let (columns, first_row, mut aligns) = record_row(first)?;
    let mut rows = alloc::vec![first_row];
    let mut row_native = alloc::vec![native_of(first)];
    for item in &items[1..] {
        let (cols, cells, cell_aligns) = record_row(item)?;
        if cols != columns {
            return None; // heterogeneous shapes don't form one table
        }
        // A column is right-aligned only if *every* cell in it is numeric; one
        // non-numeric cell demotes the whole column to left.
        for (col, cell_align) in cell_aligns.iter().enumerate() {
            if *cell_align == Align::Left {
                aligns[col] = Align::Left;
            }
        }
        rows.push(cells);
        row_native.push(native_of(item)); // provenance is per row: a mixed list
        // (real caps + a look-alike user record) colors only the real rows.
    }
    Some(Table { header: Some(columns), aligns, rows, row_native })
}

/// Whether `value` is a kernel-built record — the un-forgeable provenance the
/// colorizer keys on. User Stitch cannot set a `DataValue`'s `native` flag.
fn native_of(value: &Value) -> bool {
    matches!(value, Value::Data(data) if data.native)
}

/// The alignment a value's column wants: numbers right-justify, everything else
/// left. Read off the value's *type*, not its rendered text.
fn align_of(value: &Value) -> Align {
    match value {
        Value::Int(_) | Value::Float(_) => Align::Right,
        _ => Align::Left,
    }
}

/// A record's `(column names, cell strings, per-field alignments)`, or `None` if
/// it isn't a `Data` with all fields named.
fn record_row(value: &Value) -> Option<(Vec<String>, Vec<String>, Vec<Align>)> {
    let Value::Data(data) = value else {
        return None;
    };
    let columns = data.fields.iter().map(|(name, _)| name.clone()).collect::<Option<Vec<_>>>()?;
    let cells = data.fields.iter().map(|(_, v)| v.display()).collect();
    let aligns = data.fields.iter().map(|(_, v)| align_of(v)).collect();
    Some((columns, cells, aligns))
}

/// A single **product** record as a **headerless** key/value [`Table`] (each row
/// is `[field, value]`), or `None` if it isn't a product `Data` (a `prod`, i.e.
/// `variant == type_name`) with all fields named and at least one field. Sum
/// variants render as a tree instead (so the variant name is not lost); a nullary
/// variant (like `None`) renders as its name.
fn as_kv(value: &Value) -> Option<Table> {
    let Value::Data(data) = value else {
        return None;
    };
    if data.variant != data.type_name || data.fields.is_empty() {
        return None;
    }
    let rows = data
        .fields
        .iter()
        .map(|(name, value)| Some(alloc::vec![name.clone()?, value.display()]))
        .collect::<Option<Vec<_>>>()?;
    // Field-name column is a label (always left); the value column right-aligns
    // only when every value is numeric.
    let value_align =
        if data.fields.iter().all(|(_, v)| align_of(v) == Align::Right) {
            Align::Right
        } else {
            Align::Left
        };
    // Every row of this single record shares its provenance.
    let row_native = alloc::vec![data.native; rows.len()];
    Some(Table { header: None, aligns: alloc::vec![Align::Left, value_align], rows, row_native })
}

/// Whether `value` is a sum variant carrying fields — the tree case (a nullary
/// variant like `None` renders as its name, not a tree).
fn is_variant_with_fields(value: &Value) -> bool {
    matches!(value, Value::Data(data) if data.variant != data.type_name && !data.fields.is_empty())
}

/// Whether `value` is a `Data` holding at least one field that is *itself* a
/// record/variant — the case that trees the whole thing rather than flattening
/// the nested record into a single cell.
fn has_nested_record(value: &Value) -> bool {
    matches!(value, Value::Data(data)
        if data.fields.iter().any(|(_, v)| matches!(v, Value::Data(_))))
}

/// A `Data`'s tree label: a sum variant shows its *variant* name; a product
/// (nested inside a tree) shows its *type* name.
fn node_label(data: &DataValue) -> String {
    if data.variant == data.type_name {
        data.type_name.clone()
    } else {
        data.variant.clone()
    }
}

/// Render a value as an indented tree (`├─`/`└─`), recursing through nested `Data`.
/// Non-`Data` values (and empty ones) are leaves rendered with [`Value::display`].
fn tree(value: &Value) -> String {
    tree_lines(value).join("\n")
}

fn tree_lines(value: &Value) -> Vec<String> {
    let Value::Data(data) = value else {
        return alloc::vec![value.display()];
    };
    if data.fields.is_empty() {
        return alloc::vec![node_label(data)];
    }
    let mut lines = alloc::vec![node_label(data)];
    let last = data.fields.len() - 1;
    for (i, (name, child)) in data.fields.iter().enumerate() {
        let (branch, indent) = if i == last { ("└─ ", "   ") } else { ("├─ ", "│  ") };
        let mut sublines = tree_lines(child);
        if let Some(name) = name {
            sublines[0] = format!("{name}: {}", sublines[0]);
        }
        for (row, line) in sublines.into_iter().enumerate() {
            let prefix = if row == 0 { branch } else { indent };
            lines.push(format!("{prefix}{line}"));
        }
    }
    lines
}

/// Render `value` for the terminal against `style`: a homogeneous record *list*
/// becomes a table; a record holding a nested record, or a sum variant, an
/// indented tree; a *flat* product record a key/value table; anything else falls
/// back to [`Value::display`].
#[must_use]
pub fn render_with(value: &Value, style: &dyn TableStyle) -> String {
    if let Some(table) = as_table(value) {
        return style.render(&table);
    }
    if has_nested_record(value) {
        return tree(value);
    }
    if let Some(kv) = as_kv(value) {
        return style.render(&kv);
    }
    if is_variant_with_fields(value) {
        return tree(value);
    }
    value.display()
}

/// [`render_with`] using the default [`BoxStyle`] — the REPL's result printer.
#[must_use]
pub fn render(value: &Value) -> String {
    render_with(value, &BoxStyle::plain())
}

/// [`render_with`] a colored [`BoxStyle`] — `colorize` wraps recognized cell
/// content (rights glyphs) in ANSI. The REPL uses this on a color-capable
/// channel; [`render`] stays plain for pipes and snapshots.
#[must_use]
pub fn render_colored(value: &Value, colorize: fn(bool, &str) -> String) -> String {
    render_with(value, &BoxStyle::with_colorizer(colorize))
}

#[cfg(test)]
mod tests {
    use super::{Align, BoxStyle, Table, TableStyle, render};
    use crate::value::{DataValue, Value};
    #[allow(clippy::wildcard_imports, reason = "alloc prelude for no_std")]
    use crate::prelude::*;

    /// A record `Value` with named fields — a table row.
    fn record(fields: Vec<(&str, Value)>) -> Value {
        Value::Data(Rc::new(DataValue {
            type_name: "R".into(),
            variant: "R".into(),
            fields: fields.into_iter().map(|(n, v)| (Some(n.into()), v)).collect(),
            native: false,
        }))
    }

    /// A `Data` with an explicit `type_name`/`variant` and (optionally named)
    /// fields — for building sum variants and nested products.
    fn variant(type_name: &str, name: &str, fields: Vec<(Option<&str>, Value)>) -> Value {
        Value::Data(Rc::new(DataValue {
            type_name: type_name.into(),
            variant: name.into(),
            fields: fields.into_iter().map(|(n, v)| (n.map(Into::into), v)).collect(),
            native: false,
        }))
    }

    #[test]
    fn a_sum_variant_with_a_nested_record_renders_as_a_tree() {
        let point =
            variant("Point", "Point", vec![(Some("x"), Value::Int(1)), (Some("y"), Value::Int(2))]);
        let ok = variant("Result", "Ok", vec![(None, point)]);
        assert_eq!(
            render(&ok),
            "\
Ok
└─ Point
   ├─ x: 1
   └─ y: 2"
        );
    }

    #[test]
    fn a_flat_sum_variant_renders_as_a_tree() {
        let some = variant("Maybe", "Some", vec![(None, Value::Int(5))]);
        assert_eq!(render(&some), "Some\n└─ 5");
    }

    #[test]
    fn a_flat_product_still_renders_as_a_kv_table_not_a_tree() {
        let point = variant("Point", "Point", vec![(Some("x"), Value::Int(1))]);
        assert!(render(&point).contains('┌'), "{}", render(&point));
    }

    #[test]
    fn a_named_field_sum_variant_renders_as_a_tree_not_a_kv_table() {
        // A sum variant must not kv-table (that would drop the variant name).
        let circle = variant("Shape", "Circle", vec![(Some("r"), Value::Int(1))]);
        assert_eq!(render(&circle), "Circle\n└─ r: 1");
    }

    #[test]
    fn a_positional_field_product_renders_via_display_not_a_tree() {
        // A product with positional fields is neither a kv table nor a tree.
        let celsius = variant("Celsius", "Celsius", vec![(None, Value::Int(5))]);
        assert_eq!(render(&celsius), "Celsius(5)");
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
            native: false,
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
│  1 │ x   │
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
            native: false,
        }));
        assert_eq!(render(&none), "None");
    }

    #[test]
    fn display_width_counts_terminal_cells_including_vs16_promotion() {
        use super::display_width;
        assert_eq!(display_width(""), 0);
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("│"), 1); // box-drawing is one cell
        assert_eq!(display_width("🪴"), 2); // a plane-1 emoji is two cells
        assert_eq!(display_width("👀"), 2);
        assert_eq!(display_width("✏️"), 2); // U+270F (1) + VS16 promotes to 2
        assert_eq!(display_width("📝"), 2); // single-scalar plane-1 emoji
        assert_eq!(display_width("🪴👀📝"), 6);
    }

    #[test]
    fn an_emoji_column_keeps_every_border_line_the_same_display_width() {
        use super::display_width;
        // Cells of differing display width (4 cells vs 2) in one column: padding
        // must be by display width, or the box shears.
        let list = Value::List(
            vec![
                record(vec![("r", Value::Str("👀👀".into()))]),
                record(vec![("r", Value::Str("👀".into()))]),
            ]
            .into(),
        );
        let out = render(&list);
        let widths = out.lines().map(display_width).collect::<Vec<_>>();
        assert!(widths.iter().all(|&w| w == widths[0]), "misaligned:\n{out}\nwidths={widths:?}");
    }

    #[test]
    fn a_list_column_holding_nested_records_still_tables_and_flattens_them() {
        // Nested-record trees are scoped to a *single* record; inside a table
        // column the nested record stays flattened (multi-line cells are future
        // work), so the list still renders as a box table.
        let inner = variant("P", "P", vec![(Some("x"), Value::Int(1))]);
        let list = Value::List(vec![record(vec![("k", inner)])].into());
        let out = render(&list);
        assert!(out.starts_with('┌'), "{out}"); // a box table, not a tree
        assert!(out.contains("P(x: 1)"), "{out}"); // the nested record stayed flat
    }

    #[test]
    fn a_product_containing_a_nested_record_trees_instead_of_kv_tabling() {
        // A flat product kv-tables; once it holds a nested record the whole thing
        // trees, so the nested structure isn't flattened into one cell.
        let endpoint = variant(
            "Endpoint",
            "Endpoint",
            vec![(Some("id"), Value::Int(3)), (Some("badge"), Value::Int(7))],
        );
        let cap =
            variant("Cap", "Cap", vec![(Some("handle"), Value::Int(0)), (Some("kind"), endpoint)]);
        assert_eq!(
            render(&cap),
            "\
Cap
├─ handle: 0
└─ kind: Endpoint
   ├─ id: 3
   └─ badge: 7"
        );
    }

    #[test]
    fn a_column_mixing_numbers_and_text_stays_left_aligned() {
        // One non-numeric cell demotes the whole column to left.
        let list = Value::List(
            vec![
                record(vec![("v", Value::Int(5))]),
                record(vec![("v", Value::Str("hi".into()))]),
            ]
            .into(),
        );
        assert_eq!(
            render(&list),
            "\
┌────┐
│ v  │
├────┤
│ 5  │
│ hi │
└────┘"
        );
    }

    #[test]
    fn a_key_value_table_right_aligns_an_all_numeric_value_column() {
        let point = record(vec![("x", Value::Int(1)), ("y", Value::Int(20))]);
        assert_eq!(
            render(&point),
            "\
┌───┬────┐
│ x │  1 │
│ y │ 20 │
└───┴────┘"
        );
    }

    #[test]
    fn a_colorizer_decorates_cells_without_shifting_the_layout() {
        // A colorizer changes a cell's byte/char length (real one adds ANSI) but
        // must not change column widths: fill is computed from the *undecorated*
        // cell, and the decoration wraps the content after. Here `[..]` stands in
        // for the zero-visible-width ANSI a real colorizer would add.
        let table = Table {
            header: None,
            aligns: vec![Align::Left],
            rows: vec![vec!["ab".into()], vec!["abcd".into()]],
            row_native: vec![true, true], // native so the colorizer runs
        };
        // Column width is 4 (from "abcd"); "ab" gets 2 fill spaces, both computed
        // before the `[..]` wrap — so the fill count ignores the brackets.
        assert_eq!(
            BoxStyle::with_colorizer(|_native, s| alloc::format!("[{s}]")).render(&table),
            "\
┌──────┐
│ [ab]   │
│ [abcd] │
└──────┘"
        );
    }

    #[test]
    fn the_colorizer_only_touches_native_rows() {
        // Coloring keys on provenance: the native row is decorated, the
        // non-native row (and the header) are left alone — a user look-alike row
        // in the same table is never colored.
        let table = Table {
            header: Some(vec!["c".into()]),
            aligns: vec![Align::Left],
            rows: vec![vec!["a".into()], vec!["b".into()]],
            row_native: vec![true, false],
        };
        let out = BoxStyle::with_colorizer(|native, cell| {
            if native { alloc::format!("<{cell}>") } else { cell.to_string() }
        })
        .render(&table);
        assert!(out.contains("<a>"), "{out}"); // native row decorated
        assert!(!out.contains("<b>"), "{out}"); // non-native row untouched
        assert!(!out.contains("<c>"), "{out}"); // header never decorated
    }

    #[test]
    fn box_style_honors_per_column_alignment() {
        let table = Table {
            header: Some(vec!["a".into(), "bb".into()]),
            aligns: vec![Align::Left, Align::Right],
            rows: vec![
                vec!["1".into(), "2".into()],
                vec!["30".into(), "4".into()],
            ],
            row_native: vec![false, false],
        };
        assert_eq!(
            BoxStyle::plain().render(&table),
            "\
┌────┬────┐
│ a  │ bb │
├────┼────┤
│ 1  │  2 │
│ 30 │  4 │
└────┴────┘"
        );
    }
}
