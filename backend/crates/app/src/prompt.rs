//! Prompt construction. Kept deliberately compact: every fresh agent gets
//! exactly the task + the handoff it needs, nothing more. The static parts are
//! stable so provider-side prompt caching can kick in.
use crate::models::{Project, Task};

pub const MAX_HANDOFF: usize = 4000;
pub const MAX_DIFF: usize = 60_000;

const RESULT_CONTRACT: &str = r#"## Finish
When done, write `.queue/result.json` (create the folder if needed):
{"status":"done|failed|needs_input","summary":"max 5 lines, what changed","handoff":"max 15 lines for the next agent: current state, decisions, open points, relevant files","questions":[],"follow_up_tasks":[{"title":"…","prompt":"…"}]}
- "needs_input" only if you cannot continue without a decision from the user; list the questions.
- "follow_up_tasks" only for clearly necessary work outside this task's scope. Usually empty.
- Do not write anything else into `.queue/`."#;

fn cut(s: &str, n: usize) -> &str {
    if s.len() <= n {
        return s;
    }
    let mut end = n;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub struct Parent {
    pub title: String,
    pub handoff: Option<String>,
    pub summary: Option<String>,
}

fn rules(project: &Project) -> String {
    let mut r = format!(
        "## Rules\n- You are an autonomous coding agent working in the current directory (project \"{}\"). Nobody can answer questions during the run.\n- Explore only what the task needs; prefer targeted search over reading whole trees.\n- Verify your work (build/tests/run) before finishing.\n",
        project.name
    );
    if let Some(cmd) = project
        .verify_command
        .as_deref()
        .filter(|c| !c.trim().is_empty())
    {
        r.push_str(&format!(
            "- The queue will run `{cmd}` afterwards; it must pass.\n"
        ));
    }
    r
}

pub fn task(task: &Task, project: &Project, parent: Option<&Parent>) -> String {
    let mut p = format!("# Task: {}\n\n{}\n", task.title, task.prompt.trim());
    if !project.goal.trim().is_empty() {
        p.push_str(&format!(
            "\n## Project goal\n{}\n",
            cut(project.goal.trim(), 1500)
        ));
    }
    if let Some(par) = parent {
        let h = par
            .handoff
            .as_deref()
            .or(par.summary.as_deref())
            .unwrap_or("");
        if !h.trim().is_empty() {
            p.push_str(&format!(
                "\n## Handoff from previous task \"{}\"\n{}\n",
                par.title,
                cut(h.trim(), MAX_HANDOFF)
            ));
        }
    }
    if !task.context.trim().is_empty() {
        p.push_str(&format!("\n## Context\n{}\n", task.context.trim()));
    }
    p.push('\n');
    p.push_str(&rules(project));
    p.push('\n');
    p.push_str(RESULT_CONTRACT);
    p
}

/// Prompt for a fix task created after a failure.
pub fn fix_prompt(original_title: &str, original_prompt: &str, failure: &str) -> String {
    format!(
        "The previous attempt at task \"{original_title}\" failed. First analyze the root cause (do not guess), then fix it with a minimal, correct change.\n\n### Original task\n{}\n\n### Failure\n{}\n",
        cut(original_prompt.trim(), 6000),
        cut(failure.trim(), 6000)
    )
}

pub fn review(task: &Task, summary: &str, diff: &str) -> String {
    format!(
        "You are a strict code reviewer. Another agent just worked on the task below; its changes are in the current directory.\n\n### Task\n{}\n\n### Agent summary\n{}\n\n### Diff\n```diff\n{}\n```\n\nCheck: is the task fulfilled, are there bugs, security issues or broken builds? Inspect files only where the diff is unclear. Do NOT modify any files except `.queue/review.json`.\nWrite `.queue/review.json`: {{\"approved\":true|false,\"issues\":[\"concrete, actionable issue\", …]}}\nApprove unless there are real defects or the task is not fulfilled; style nits are not defects.",
        cut(task.prompt.trim(), 6000),
        cut(summary.trim(), 2000),
        cut(diff, MAX_DIFF)
    )
}

pub fn onempty(
    project: &Project,
    actions: &[String],
    history: &[(String, Option<String>)],
) -> String {
    let mut p = String::from("# Maintenance run (the task queue is empty)\n\nDo NOT change project files. Your job is to find the most valuable next work and propose it.\n\n");
    for a in actions {
        match a.as_str() {
            "review" => p.push_str("- **Review**: look for bugs, security issues, missing tests and maintainability problems in the code.\n"),
            "research" => p.push_str("- **Research**: search the web for relevant new library versions, tools, techniques or best practices that this project could benefit from (cite sources in the prompt you propose).\n"),
            "improve" => p.push_str("- **Improve**: analyze the recent work below and suggest concrete improvements or next steps towards the project goal.\n"),
            _ => {}
        }
    }
    if !project.goal.trim().is_empty() {
        p.push_str(&format!(
            "\n## Project goal\n{}\n",
            cut(project.goal.trim(), 1500)
        ));
    }
    if !history.is_empty() {
        p.push_str("\n## Recent tasks\n");
        for (t, s) in history {
            p.push_str(&format!(
                "- {t}: {}\n",
                cut(s.as_deref().unwrap_or("").lines().next().unwrap_or(""), 200)
            ));
        }
    }
    p.push_str("\n## Output\nPropose at most 5 tasks, most valuable first, each with a self-contained prompt (a fresh agent will execute it without your context). If you need a decision from the user, set status \"needs_input\" and ask. If nothing is worth doing, return no tasks.\n\n");
    p.push_str(RESULT_CONTRACT);
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn project() -> Project {
        Project {
            id: Uuid::nil(),
            owner_id: Uuid::nil(),
            name: "Demo".into(),
            description: String::new(),
            goal: "Ship v1".into(),
            verify_command: Some("cargo test".into()),
            review_enabled: true,
            review_provider: None,
            review_model: None,
            created_at: Utc::now(),
            git_remote_url: None,
            git_branch: "main".into(),
            git_token_enc: None,
            auto_push: false,
        }
    }

    fn task(prompt: &str, context: &str) -> Task {
        Task {
            id: Uuid::nil(),
            user_id: Uuid::nil(),
            project_id: Uuid::nil(),
            parent_id: None,
            kind: "task".into(),
            title: "T".into(),
            prompt: prompt.into(),
            context: context.into(),
            provider: "claude".into(),
            model: None,
            status: "queued".into(),
            position: 0,
            attempt: 0,
            depth: 0,
            created_by_agent: false,
            questions: None,
            handoff: None,
            summary: None,
            error: None,
            review: None,
            usage: None,
            snapshot_before: None,
            snapshot_after: None,
            created_at: Utc::now(),
            started_at: None,
            finished_at: None,
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn task_prompt_contains_goal_handoff_context_rules_and_contract() {
        let parent = Parent {
            title: "Prev".into(),
            handoff: Some("state X".into()),
            summary: None,
        };
        let p = super::task(&task("Do Y", "use lib Z"), &project(), Some(&parent));
        assert!(p.starts_with("# Task: T\n\nDo Y"));
        for needle in [
            "Ship v1",
            "Handoff from previous task \"Prev\"",
            "state X",
            "use lib Z",
            "`cargo test`",
            ".queue/result.json",
        ] {
            assert!(p.contains(needle), "missing {needle}");
        }
    }

    #[test]
    fn handoff_is_capped() {
        let big = "h".repeat(MAX_HANDOFF * 3);
        let parent = Parent {
            title: "P".into(),
            handoff: Some(big),
            summary: None,
        };
        let p = super::task(&task("x", ""), &project(), Some(&parent));
        assert!(
            p.len() < MAX_HANDOFF + 3000,
            "prompt too large: {}",
            p.len()
        );
    }

    #[test]
    fn parent_summary_is_fallback_for_missing_handoff() {
        let parent = Parent {
            title: "P".into(),
            handoff: None,
            summary: Some("did things".into()),
        };
        assert!(super::task(&task("x", ""), &project(), Some(&parent)).contains("did things"));
    }

    #[test]
    fn static_rules_are_identical_across_tasks_for_prompt_caching() {
        let a = super::task(&task("a", ""), &project(), None);
        let b = super::task(&task("b", ""), &project(), None);
        let tail = |s: &str| s[s.find("## Rules").unwrap()..].to_string();
        assert_eq!(tail(&a), tail(&b));
    }

    #[test]
    fn review_prompt_truncates_large_diffs() {
        let diff = "+x\n".repeat(MAX_DIFF);
        let p = review(&task("x", ""), "sum", &diff);
        assert!(p.len() < MAX_DIFF + 10_000);
        assert!(p.contains(".queue/review.json"));
    }

    #[test]
    fn onempty_prompt_lists_only_selected_actions_and_forbids_changes() {
        let p = onempty(
            &project(),
            &["research".into()],
            &[("Old".into(), Some("did it\nmore".into()))],
        );
        assert!(p.contains("Research"));
        assert!(!p.contains("**Review**"));
        assert!(p.contains("Do NOT change project files"));
        assert!(p.contains("- Old: did it"));
        assert!(!p.contains("more"), "only first summary line");
    }

    #[test]
    fn fix_prompt_demands_root_cause() {
        let p = fix_prompt("T", "orig", "boom");
        assert!(p.contains("root cause") && p.contains("orig") && p.contains("boom"));
    }

    #[test]
    fn cut_respects_utf8_boundaries() {
        assert_eq!(cut("äöü", 3), "ä");
    }
}
