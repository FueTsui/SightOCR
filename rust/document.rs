//! Structured recognition results. Text stays the editable form; a table also
//! keeps its grid and merged cells so the clipboard can offer an HTML copy that
//! spreadsheets and word processors paste with merges intact.

use serde::{Deserialize, Serialize};

/// A merged cell: its top-left slot covers `row_span` x `col_span` slots.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub row_span: usize,
    pub col_span: usize,
}

/// A recognized table. `grid` is exactly what the TSV shows. `merges` never
/// overlap, never leave the grid and never cover text other than their own
/// top-left slot; every other slot is a single cell. Only merges are stored so
/// a large, sparse grid stays small on the worker pipe.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    /// Title rows above and note rows below a framed table.
    pub above: Vec<String>,
    pub below: Vec<String>,
    pub grid: Vec<Vec<String>>,
    pub merges: Vec<TableCell>,
}

impl Table {
    /// Build a table from a grid and candidate merges. Merges that leave the
    /// grid, overlap an earlier merge or would hide text are trimmed.
    pub fn new(
        above: Vec<String>,
        below: Vec<String>,
        mut grid: Vec<Vec<String>>,
        merges: impl IntoIterator<Item = TableCell>,
    ) -> Self {
        let cols = grid.iter().map(Vec::len).max().unwrap_or(0);
        for row in &mut grid {
            row.resize(cols, String::new());
        }
        let rows = grid.len();
        let mut merges: Vec<_> = merges
            .into_iter()
            .filter(|cell| cell.row < rows && cell.col < cols)
            .collect();
        merges.sort_by_key(|cell| (cell.row, cell.col));
        let mut covered = vec![vec![false; cols]; rows];
        let free =
            |covered: &[Vec<bool>], r: usize, c: usize| !covered[r][c] && grid[r][c].is_empty();
        let mut kept = Vec::new();
        for merge in merges {
            let (r, c) = (merge.row, merge.col);
            if covered[r][c] {
                continue;
            }
            let col_span = 1
                + (c + 1..(c + merge.col_span.max(1)).min(cols))
                    .take_while(|&cc| free(&covered, r, cc))
                    .count();
            let row_span = 1
                + (r + 1..(r + merge.row_span.max(1)).min(rows))
                    .take_while(|&rr| (c..c + col_span).all(|cc| free(&covered, rr, cc)))
                    .count();
            if row_span * col_span == 1 {
                continue;
            }
            for row in &mut covered[r..r + row_span] {
                row[c..c + col_span].fill(true);
            }
            kept.push(TableCell {
                row: r,
                col: c,
                row_span,
                col_span,
            });
        }
        Self {
            above,
            below,
            grid,
            merges: kept,
        }
    }

    pub fn to_tsv(&self) -> String {
        self.above
            .iter()
            .cloned()
            .chain(self.grid.iter().map(|row| row.join("\t")))
            .chain(self.below.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn write_html(&self, out: &mut String) {
        for line in &self.above {
            paragraph(out, line);
        }
        if !self.grid.is_empty() {
            // Slots hidden under a merge, and the spans of each merge's anchor.
            let cols = self.grid.iter().map(Vec::len).max().unwrap_or(0);
            let mut hidden = vec![vec![false; cols]; self.grid.len()];
            let mut spans = std::collections::HashMap::new();
            for merge in &self.merges {
                let rows =
                    merge.row.min(hidden.len())..(merge.row + merge.row_span).min(hidden.len());
                let span = merge.col.min(cols)..(merge.col + merge.col_span).min(cols);
                for row in &mut hidden[rows] {
                    row[span.clone()].fill(true);
                }
                spans.insert((merge.row, merge.col), (merge.row_span, merge.col_span));
            }
            out.push_str("<table border=\"1\" style=\"border-collapse:collapse\">");
            for (r, row) in self.grid.iter().enumerate() {
                out.push_str("<tr>");
                for (c, text) in row.iter().enumerate() {
                    let (row_span, col_span) = spans.get(&(r, c)).copied().unwrap_or((1, 1));
                    if hidden[r][c] && row_span * col_span == 1 {
                        continue;
                    }
                    out.push_str("<td");
                    if row_span > 1 {
                        out.push_str(&format!(" rowspan=\"{row_span}\""));
                    }
                    if col_span > 1 {
                        out.push_str(&format!(" colspan=\"{col_span}\""));
                    }
                    if keeps_as_text(text) {
                        // Excel would drop leading zeros or round ID numbers.
                        out.push_str(" style=\"mso-number-format:'\\@'\"");
                    }
                    out.push('>');
                    escape(out, text);
                    out.push_str("</td>");
                }
                out.push_str("</tr>");
            }
            out.push_str("</table>");
        }
        for line in &self.below {
            paragraph(out, line);
        }
    }
}

/// The text form of several tables, as table recognition has always shown it.
pub fn tables_tsv(tables: &[Table]) -> String {
    tables
        .iter()
        .map(Table::to_tsv)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// An HTML fragment for the clipboard's "HTML Format".
pub fn tables_html(tables: &[Table]) -> String {
    let mut out = String::new();
    for (index, table) in tables.iter().enumerate() {
        if index > 0 {
            out.push_str("<p></p>");
        }
        table.write_html(&mut out);
    }
    out
}

/// Digit strings that Excel would turn into numbers and damage: leading
/// zeros (`007`) or 12+ digits (ID and bank card numbers lose precision).
fn keeps_as_text(text: &str) -> bool {
    let text = text.trim();
    text.bytes().all(|b| b.is_ascii_digit())
        && (text.len() >= 12 || (text.len() >= 2 && text.starts_with('0')))
}

fn paragraph(out: &mut String, text: &str) {
    out.push_str("<p>");
    escape(out, text);
    out.push_str("</p>");
}

fn escape(out: &mut String, text: &str) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(ch),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(rows: &[&[&str]]) -> Vec<Vec<String>> {
        rows.iter()
            .map(|row| row.iter().map(|s| (*s).to_owned()).collect())
            .collect()
    }

    fn merge(row: usize, col: usize, row_span: usize, col_span: usize) -> TableCell {
        TableCell {
            row,
            col,
            row_span,
            col_span,
        }
    }

    #[test]
    fn merges_are_trimmed_to_free_slots() {
        let table = Table::new(
            vec![],
            vec![],
            grid(&[&["A", "", "B"], &["", "C", ""], &["D"]]),
            // Overlapping, out-of-range and text-hiding merges are trimmed.
            [
                merge(0, 0, 2, 2),
                merge(0, 2, 3, 1),
                merge(9, 0, 1, 1),
                merge(1, 0, 2, 1),
            ],
        );
        // "C" stops the 2x2 merge growing down and "D" the last one; the
        // out-of-range merge is dropped.
        assert_eq!(table.merges, [merge(0, 0, 1, 2), merge(0, 2, 3, 1)]);
        assert_eq!(table.to_tsv(), "A\t\tB\n\tC\t\nD\t\t");
        assert_eq!(
            tables_html(&[table]),
            "<table border=\"1\" style=\"border-collapse:collapse\">\
             <tr><td colspan=\"2\">A</td><td rowspan=\"3\">B</td></tr>\
             <tr><td></td><td>C</td></tr><tr><td>D</td><td></td></tr></table>"
        );
    }

    #[test]
    fn a_large_sparse_table_stays_small_on_the_worker_pipe() {
        let mut cells = vec![vec![String::new(); 512]; 512];
        cells[0][0] = "A".into();
        let table = Table::new(vec![], vec![], cells, [merge(0, 0, 512, 512)]);
        assert_eq!(table.merges, [merge(0, 0, 512, 512)]);
        let json = serde_json::to_vec(&table).unwrap();
        assert!(json.len() < 1024 * 1024, "{} bytes", json.len());
        // One cell plus 511 empty <tr></tr> rows.
        assert!(tables_html(&[table]).len() < 8 * 1024);
    }

    #[test]
    fn html_keeps_merges_and_id_numbers() {
        let table = Table::new(
            vec!["标题 <1>".into()],
            vec!["注：单位 & 元".into()],
            grid(&[
                &["部门", "", "编号"],
                &["销售", "华东", "007"],
                &["", "华南", "110101199003071234"],
            ]),
            [merge(0, 0, 1, 2), merge(1, 0, 2, 1)],
        );
        let html = tables_html(std::slice::from_ref(&table));
        assert_eq!(
            html,
            "<p>标题 &lt;1&gt;</p><table border=\"1\" style=\"border-collapse:collapse\">\
             <tr><td colspan=\"2\">部门</td><td>编号</td></tr>\
             <tr><td rowspan=\"2\">销售</td><td>华东</td>\
             <td style=\"mso-number-format:'\\@'\">007</td></tr>\
             <tr><td>华南</td><td style=\"mso-number-format:'\\@'\">110101199003071234</td></tr>\
             </table><p>注：单位 &amp; 元</p>"
        );
        assert_eq!(
            tables_tsv(&[table.clone(), table]).matches("\n\n").count(),
            1
        );
    }

    #[test]
    fn only_damaging_digit_strings_stay_text() {
        for text in ["007", "0", "123456789012", " 00 "] {
            assert_eq!(keeps_as_text(text), text.trim() != "0", "{text}");
        }
        for text in [
            "1234",
            "12345678901",
            "1,234",
            "-8.6%",
            "0.5",
            "XM-0457",
            "",
        ] {
            assert!(!keeps_as_text(text), "{text}");
        }
    }
}
