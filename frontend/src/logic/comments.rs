//! Line selection and placement of inline diff comments (WP06). Pure logic
//! on top of the shared diff parser (`queued_api::diff`).
use std::collections::BTreeMap;

use queued_api::diff::{lines_of, File};
use queued_api::steer::{CommentLocation, CommentView, Side};

/// Selected lines of one file and side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub path: String,
    pub side: Side,
    pub start: i32,
    pub end: i32,
}

impl Selection {
    pub fn contains(&self, path: &str, side: Side, n: i32) -> bool {
        self.path == path && self.side == side && (self.start..=self.end).contains(&n)
    }

    pub fn label(&self) -> String {
        let side = match self.side {
            Side::New => "",
            Side::Old => " (entfernt)",
        };
        if self.start == self.end {
            format!("Zeile {}{side} in {}", self.start, self.path)
        } else {
            format!("Zeilen {}–{}{side} in {}", self.start, self.end, self.path)
        }
    }
}

/// A tap/click on a line: starts a selection, extends a single line to a
/// range in the same file, side and section, or starts over.
pub fn click(
    current: Option<&Selection>,
    files: &[File],
    path: &str,
    side: Side,
    n: i32,
) -> Result<Selection, &'static str> {
    let single = Selection {
        path: path.to_string(),
        side,
        start: n,
        end: n,
    };
    let Some(c) = current else {
        return Ok(single);
    };
    if c.path != path || c.side != side || c.start != c.end || c.start == n {
        return Ok(single);
    }
    let (start, end) = (c.start.min(n), c.start.max(n));
    if end - start + 1 > queued_api::steer::MAX_COMMENT_LINES {
        return Err("Ein Kommentar umfasst höchstens 200 Zeilen");
    }
    let f = files
        .iter()
        .find(|f| f.path == path)
        .ok_or("Datei nicht gefunden")?;
    if lines_of(f, side, start, end).is_none() {
        return Err("Ein Bereich muss in einem zusammenhängenden Abschnitt liegen");
    }
    Ok(Selection {
        path: path.to_string(),
        side,
        start,
        end,
    })
}

/// Comments under a line: (file, side, line) → indices into the list.
pub type Placed = BTreeMap<(String, Side, i32), Vec<usize>>;

/// Where comments appear: under the last line they refer to now, or in the
/// list of stale comments.
pub fn placement(comments: &[CommentView]) -> (Placed, Vec<usize>) {
    let mut at: Placed = BTreeMap::new();
    let mut stale = vec![];
    for (i, c) in comments.iter().enumerate() {
        let end = match c.location {
            CommentLocation::Current => c.line_end,
            CommentLocation::Moved { line_end, .. } => line_end,
            CommentLocation::Stale => {
                stale.push(i);
                continue;
            }
        };
        at.entry((c.path.clone(), c.side, end)).or_default().push(i);
    }
    (at, stale)
}

/// "3 Kommentare" / "1 Kommentar".
pub fn count_label(n: usize) -> String {
    if n == 1 {
        "1 Kommentar".into()
    } else {
        format!("{n} Kommentare")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use queued_api::diff::parse;

    const D: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@\n a\n-b\n+c\n+d\n e\n@@ -10,1 +11,1 @@\n-x\n+y\n";

    #[test]
    fn taps_select_extend_and_start_over() {
        let f = parse(D);
        let s = click(None, &f, "a.rs", Side::New, 2).unwrap();
        assert_eq!((s.start, s.end), (2, 2));
        assert_eq!(s.label(), "Zeile 2 in a.rs");
        // Second tap below and above: a range.
        let r = click(Some(&s), &f, "a.rs", Side::New, 4).unwrap();
        assert_eq!((r.start, r.end), (2, 4));
        assert_eq!(r.label(), "Zeilen 2–4 in a.rs");
        assert!(r.contains("a.rs", Side::New, 3) && !r.contains("a.rs", Side::Old, 3));
        let r2 = click(
            Some(&click(None, &f, "a.rs", Side::New, 4).unwrap()),
            &f,
            "a.rs",
            Side::New,
            1,
        )
        .unwrap();
        assert_eq!((r2.start, r2.end), (1, 4));
        // A third tap starts over; another side starts over.
        let n = click(Some(&r), &f, "a.rs", Side::New, 3).unwrap();
        assert_eq!((n.start, n.end), (3, 3));
        let o = click(Some(&s), &f, "a.rs", Side::Old, 2).unwrap();
        assert_eq!((o.side, o.start), (Side::Old, 2));
        assert_eq!(o.label(), "Zeile 2 (entfernt) in a.rs");
        // Across sections: refused with an explanation.
        assert!(click(Some(&s), &f, "a.rs", Side::New, 11).is_err());
        // Tapping the selected line again keeps a single line.
        assert_eq!(click(Some(&s), &f, "a.rs", Side::New, 2).unwrap(), s);
    }

    fn view(path: &str, end: i32, loc: CommentLocation) -> CommentView {
        CommentView {
            id: uuid::Uuid::nil(),
            task_id: uuid::Uuid::nil(),
            author_id: uuid::Uuid::nil(),
            author: "a".into(),
            path: path.into(),
            side: Side::New,
            line_start: end,
            line_end: end,
            body: "b".into(),
            anchor: vec![],
            sent: false,
            location: loc,
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            sent_task: None,
            mine: true,
        }
    }

    #[test]
    fn comments_sit_under_their_current_line_or_in_the_stale_list() {
        let cs = vec![
            view("a.rs", 2, CommentLocation::Current),
            view(
                "a.rs",
                2,
                CommentLocation::Moved {
                    line_start: 5,
                    line_end: 6,
                },
            ),
            view("a.rs", 3, CommentLocation::Stale),
            view("a.rs", 2, CommentLocation::Current),
        ];
        let (at, stale) = placement(&cs);
        assert_eq!(at[&("a.rs".to_string(), Side::New, 2)], vec![0, 3]);
        assert_eq!(at[&("a.rs".to_string(), Side::New, 6)], vec![1]);
        assert_eq!(stale, vec![2]);
        assert_eq!(count_label(1), "1 Kommentar");
        assert_eq!(count_label(3), "3 Kommentare");
    }
}
