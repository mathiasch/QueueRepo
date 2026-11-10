//! Branch per task and merge queue (WP04): texts and tones for the UI.
use queued_api::branch::MergeState;
use queued_api::Task;

use super::labels::Tone;

pub fn merge_tone(m: MergeState) -> Tone {
    match m {
        MergeState::Queued | MergeState::Merging => Tone::Info,
        MergeState::Merged | MergeState::Published => Tone::Success,
        MergeState::Conflict | MergeState::Failed => Tone::Danger,
    }
}

/// Place in the merge queue in plain words (1 = being merged or next).
pub fn position_text(state: MergeState, position: u32) -> String {
    match (state, position) {
        (MergeState::Merging, _) => "Wird gerade übernommen".into(),
        (_, 0 | 1) => "Als Nächstes dran".into(),
        (_, n) => format!("Platz {n} in der Übernahme-Warteschlange"),
    }
}

/// First 8 characters of a commit id.
pub fn short_sha(s: &str) -> String {
    s.chars().take(8).collect()
}

/// What the drawer says about the task's branch (`None` = task from before
/// branches existed).
pub fn branch_lines(t: &Task) -> Option<Vec<String>> {
    let branch = t.branch.as_deref()?;
    let mut lines = vec![format!("Branch: {branch}")];
    if let Some(b) = t.merge_base.as_deref() {
        let label = if t.on_target() {
            "Übernommen auf Stand"
        } else if t.pr_id.is_some() {
            "Gestartet auf Stand des Pull Requests"
        } else {
            "Abgezweigt von Stand"
        };
        lines.push(format!("{label} {}", short_sha(b)));
    }
    if t.carry_from.is_some() {
        lines.push(
            "Setzt die Änderungen des fehlgeschlagenen Auftrags fort (auf dem neuesten Stand)."
                .into(),
        );
    }
    Some(lines)
}

/// Whether the drawer may offer "Rückgängig machen": the changes are on the
/// target branch and there is a diff.
pub fn can_revert(t: &Task) -> bool {
    t.has_changes() && t.on_target()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn task(extra: serde_json::Value) -> Task {
        let mut v = json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "user_id": "00000000-0000-0000-0000-000000000002",
            "project_id": "00000000-0000-0000-0000-000000000003",
            "parent_id": null, "kind": "task", "title": "t", "prompt": "p", "context": "",
            "provider": "mock", "model": null, "status": "merging", "position": 1,
            "attempt": 0, "depth": 0, "created_by_agent": false, "questions": null,
            "handoff": null, "summary": null, "error": null, "review": null, "usage": null,
            "snapshot_before": "aaaaaaaaaaaa", "snapshot_after": "bbbbbbbbbbbb",
            "created_at": "2026-01-01T10:00:00Z", "started_at": null, "finished_at": null,
            "updated_at": "2026-01-01T10:00:00Z", "max_runtime_secs": null, "max_cost_usd": null,
            "max_tokens": null, "max_turns": null, "failure_class": null,
            "transient_retries": 0, "not_before": null
        });
        for (k, val) in extra.as_object().unwrap() {
            v[k] = val.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn positions_and_tones() {
        assert_eq!(
            position_text(MergeState::Merging, 1),
            "Wird gerade übernommen"
        );
        assert_eq!(position_text(MergeState::Queued, 1), "Als Nächstes dran");
        assert_eq!(
            position_text(MergeState::Queued, 3),
            "Platz 3 in der Übernahme-Warteschlange"
        );
        assert_eq!(merge_tone(MergeState::Merged), Tone::Success);
        assert_eq!(merge_tone(MergeState::Conflict), Tone::Danger);
        assert_eq!(merge_tone(MergeState::Queued), Tone::Info);
        assert_eq!(short_sha("0123456789abcdef"), "01234567");
    }

    #[test]
    fn branch_texts_and_revert() {
        // Before WP04: no branch block, revert as before.
        let old = task(json!({"status": "done"}));
        assert!(branch_lines(&old).is_none());
        assert!(can_revert(&old));
        // Waiting for its merge: branch and fork point, no revert.
        let t = task(
            json!({"branch": "queue/0123456789ab", "merge_base": "cccccccccccc", "merge_state": "queued"}),
        );
        let l = branch_lines(&t).unwrap();
        assert_eq!(l[0], "Branch: queue/0123456789ab");
        assert_eq!(l[1], "Abgezweigt von Stand cccccccc");
        assert!(!can_revert(&t));
        // Merged: revert possible.
        let m = task(
            json!({"branch": "queue/0123456789ab", "merge_base": "cccccccccccc", "merge_state": "merged", "status": "done"}),
        );
        assert_eq!(
            branch_lines(&m).unwrap()[1],
            "Übernommen auf Stand cccccccc"
        );
        assert!(can_revert(&m));
        let f = task(
            json!({"branch": "queue/0123456789ab", "carry_from": "00000000-0000-0000-0000-000000000009"}),
        );
        assert!(branch_lines(&f)
            .unwrap()
            .iter()
            .any(|l| l.contains("fortsetzt") || l.contains("Setzt")));
    }
}
