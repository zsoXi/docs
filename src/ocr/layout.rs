//! Vertical whitespace separates prose columns; short aligned cells remain tables.
use super::Line;
use crate::read::ocr_layout::{Word, markdown as rows};

fn rect(line: &Line) -> [f64; 4] {
    [
        line.bbox
            .iter()
            .map(|p| f64::from(p[0]))
            .fold(f64::INFINITY, f64::min),
        line.bbox
            .iter()
            .map(|p| f64::from(p[1]))
            .fold(f64::INFINITY, f64::min),
        line.bbox.iter().map(|p| f64::from(p[0])).fold(0., f64::max),
        line.bbox.iter().map(|p| f64::from(p[1])).fold(0., f64::max),
    ]
}

fn render(lines: &[&Line]) -> String {
    rows(
        lines
            .iter()
            .map(|line| {
                let [left, top, right, bottom] = rect(line);
                Word {
                    text: line.text.clone(),
                    left,
                    right,
                    top,
                    bottom,
                }
            })
            .collect(),
    )
}

pub(super) fn markdown(lines: &[Line]) -> String {
    let all: Vec<_> = lines.iter().collect();
    if lines.len() < 6 {
        return render(&all);
    }
    let mut widths: Vec<_> = lines
        .iter()
        .filter(|l| l.text.chars().count() >= 10)
        .map(|l| {
            let r = rect(l);
            r[2] - r[0]
        })
        .collect();
    widths.sort_by(f64::total_cmp);
    let Some(&width) = widths.get(widths.len() / 2) else {
        return render(&all);
    };
    // Full-width headings/footers divide the page into independent column bands.
    // Never let one heading fill the gutter of the prose beneath it.
    let mut spanners: Vec<_> = lines
        .iter()
        .filter(|l| {
            let r = rect(l);
            r[2] - r[0] > width * 1.8
        })
        .collect();
    spanners.sort_by(|a, b| rect(a)[1].total_cmp(&rect(b)[1]));
    if !spanners.is_empty() && spanners.len() < lines.len() / 2 {
        let mut remaining: Vec<_> = lines
            .iter()
            .filter(|l| !spanners.iter().any(|s| std::ptr::eq(*s, *l)))
            .cloned()
            .collect();
        let mut blocks = Vec::new();
        for spanner in spanners {
            let (before, after): (Vec<_>, Vec<_>) = remaining
                .into_iter()
                .partition(|l| rect(l)[1] < rect(spanner)[1]);
            if !before.is_empty() {
                blocks.push(markdown(&before));
            }
            blocks.push(render(&[spanner]));
            remaining = after;
        }
        if !remaining.is_empty() {
            blocks.push(markdown(&remaining));
        }
        return blocks.join("\n\n");
    }
    let mut intervals: Vec<_> = lines.iter().map(rect).map(|r| (r[0], r[2])).collect();
    intervals.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut right = intervals[0].1;
    let mut split = None;
    let mut largest = width * 0.8;
    for (left, end) in intervals.iter().skip(1) {
        if left - right > largest {
            largest = left - right;
            split = Some((left + right) / 2.);
        }
        right = right.max(*end);
    }
    if let Some(split) = split {
        let (left, right): (Vec<_>, Vec<_>) = all.iter().copied().partition(|l| rect(l)[2] < split);
        // Table cells tend to be short. Both sides need sustained prose, not
        // a column of labels paired with amounts, before changing row order.
        let prose = |side: &[&Line]| {
            side.len() >= 3
                && side.iter().map(|l| l.text.chars().count()).sum::<usize>() >= side.len() * 12
        };
        if prose(&left) && prose(&right) {
            return format!("{}\n\n{}", render(&left), render(&right));
        }
    }
    render(&all)
}
