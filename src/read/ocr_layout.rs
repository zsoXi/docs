//! Conservative row-major reconstruction for upright OCR words.

#[derive(Debug)]
pub(crate) struct Word {
    pub text: String,
    pub left: f64,
    pub right: f64,
    pub top: f64,
    pub bottom: f64,
}

struct Row {
    words: Vec<Word>,
    top: f64,
    bottom: f64,
}

impl Row {
    fn height(&self) -> f64 {
        (self.bottom - self.top).max(1.0)
    }

    fn cells(&self) -> Vec<(f64, String)> {
        let mut cells: Vec<(f64, String)> = Vec::new();
        let mut right = f64::NEG_INFINITY;
        for word in &self.words {
            if word.left - right > self.height() * 1.5 || cells.is_empty() {
                cells.push((word.left, word.text.clone()));
            } else if let Some((_, text)) = cells.last_mut() {
                text.push(' ');
                text.push_str(&word.text);
            }
            right = word.right;
        }
        cells
    }
}

pub(crate) fn markdown(mut words: Vec<Word>) -> String {
    words.sort_by(|a, b| a.top.total_cmp(&b.top).then(a.left.total_cmp(&b.left)));
    let mut rows: Vec<Row> = Vec::new();
    for word in words {
        // Compare with the row's vertical envelope, not the OCR engine's column order.
        let row = rows.iter_mut().rev().find(|row| {
            let overlap = row.bottom.min(word.bottom) - row.top.max(word.top);
            overlap >= 0.5 * row.height().min((word.bottom - word.top).max(1.0))
        });
        if let Some(row) = row {
            row.top = row.top.min(word.top);
            row.bottom = row.bottom.max(word.bottom);
            row.words.push(word);
        } else {
            rows.push(Row {
                top: word.top,
                bottom: word.bottom,
                words: vec![word],
            });
        }
    }
    rows.sort_by(|a, b| a.top.total_cmp(&b.top));
    for row in &mut rows {
        row.words.sort_by(|a, b| a.left.total_cmp(&b.left));
    }
    let cells: Vec<_> = rows.iter().map(Row::cells).collect();
    let mut out = String::new();
    let mut at = 0;
    while at < rows.len() {
        let mut end = at + 1;
        if cells[at].len() >= 2 {
            while end < rows.len()
                && cells[end].len() == cells[at].len()
                && rows[end].top - rows[end - 1].bottom <= rows[end].height() * 2.5
                && cells[end].iter().zip(&cells[at]).all(|(a, b)| {
                    (a.0 - b.0).abs() <= rows[end].height().max(rows[at].height()) * 0.75
                })
            {
                end += 1;
            }
        }
        // A header plus two data rows avoids mistaking one prose line for a table.
        if end - at >= 3 {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            let table: Vec<_> = cells[at..end]
                .iter()
                .map(|row| row.iter().map(|(_, text)| super::escape(text)).collect())
                .collect();
            out.push_str(&super::table(&table));
            at = end;
        } else {
            let text = rows[at]
                .words
                .iter()
                .map(|w| w.text.as_str())
                .collect::<Vec<_>>()
                .join(" ");
            if !out.is_empty() {
                let gap = if at > 0 {
                    rows[at].top - rows[at - 1].bottom
                } else {
                    0.0
                };
                let list = |line: &str| {
                    line.starts_with("- ")
                        || line.starts_with("• ")
                        || line.split_once(". ").is_some_and(|(n, _)| {
                            !n.is_empty() && n.chars().all(|c| c.is_ascii_digit())
                        })
                };
                let tight_list = list(&text)
                    && out.lines().last().is_some_and(list)
                    && gap <= rows[at].height() * 2.5;
                out.push_str(if !tight_list && gap > rows[at].height() * 0.8 {
                    "\n\n"
                } else {
                    "\n"
                });
            }
            if let Some(rest) = text.strip_prefix("• ") {
                out.push_str("- ");
                out.push_str(rest);
            } else {
                out.push_str(&text);
            }
            at += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_major_words_become_rows_without_treating_prose_as_a_table() {
        let mut words = Vec::new();
        for (left, values) in [
            (20.0, ["Region", "North", "South"]),
            (220.0, ["Q2", "1.9", "2.3"]),
            (420.0, ["Q3", "2.1", "2.8"]),
        ] {
            for (row, text) in values.iter().enumerate() {
                words.push(Word {
                    text: (*text).into(),
                    left,
                    right: left + 60.0,
                    top: 100.0 + row as f64 * 30.0,
                    bottom: 120.0 + row as f64 * 30.0,
                });
            }
        }
        words.push(Word {
            text: "Paragraph.".into(),
            left: 20.0,
            right: 120.0,
            top: 10.0,
            bottom: 30.0,
        });
        assert_eq!(
            markdown(words),
            "Paragraph.\n\n| Region | Q2 | Q3 |\n| --- | --- | --- |\n| North | 1.9 | 2.1 |\n| South | 2.3 | 2.8 |"
        );
    }
}
