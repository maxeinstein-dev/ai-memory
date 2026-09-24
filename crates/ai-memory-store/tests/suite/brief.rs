//! `ai_memory_store::brief`: the char-budgeted session-start briefing
//! renderer, moved here from `ai-memory-hooks::router` (#176 follow-up) so
//! `ai-memory-web`'s briefing preview and the hook use the exact same
//! function instead of two copies that could drift apart.

use ai_memory_store::brief::{
    BRIEF_BUDGET_DEFAULT, BRIEF_BUDGET_MAX, BRIEF_BUDGET_MIN, BRIEF_CORE_PAGES_LIMIT,
    BRIEF_RECENT_PAGES_LIMIT, UNTRUSTED_HISTORY_END, UNTRUSTED_HISTORY_START, clamp_brief_budget,
    render_session_brief,
};

#[test]
fn clamp_usa_padrao_sem_pedido_e_respeita_limites() {
    assert_eq!(clamp_brief_budget(None), BRIEF_BUDGET_DEFAULT);
    assert_eq!(clamp_brief_budget(Some("abc")), BRIEF_BUDGET_DEFAULT);
    assert_eq!(clamp_brief_budget(Some("10")), BRIEF_BUDGET_MIN);
    assert_eq!(clamp_brief_budget(Some("999999")), BRIEF_BUDGET_MAX);
    assert_eq!(clamp_brief_budget(Some(" 6000 ")), 6000);
}

/// The brief renderer respects the char budget: an over-budget body is
/// truncated with a visible note, fully crowded-out core pages are
/// listed as omitted, and an empty project renders nothing at all.
#[test]
fn render_session_brief_enforces_budget() {
    let core = vec![
        ai_memory_store::BriefPageBody {
            path: "_rules/a.md".into(),
            title: "a".into(),
            body: "x".repeat(2_000),
            pinned: true,
            updated_at: "2026-07-12T00:00:00Z".into(),
        },
        ai_memory_store::BriefPageBody {
            path: "_rules/b.md".into(),
            title: "b".into(),
            body: "never truncated into view".into(),
            pinned: false,
            updated_at: "2026-07-12T00:00:00Z".into(),
        },
    ];
    let recent = vec![ai_memory_store::BriefingPage {
        path: "concepts/q.md".into(),
        title: "queue".into(),
        kind: "fact".into(),
        updated_at: "2026-07-12T00:00:00Z".into(),
    }];

    let out = render_session_brief(&core, &recent, BRIEF_BUDGET_MIN).unwrap();
    assert!(out.contains(ai_memory_core::UNTRUSTED_MEMORY_NOTICE));
    assert!(out.contains("ai-memory:untrusted-history:start"));
    assert!(out.contains("ai-memory:untrusted-history:end"));
    assert!(
        out.contains("[truncated by `[briefing] max_chars`]"),
        "over-budget body must be visibly truncated: {out}"
    );
    assert!(
        out.contains("Core pages omitted by budget") && out.contains("`_rules/b.md`"),
        "crowded-out core pages must be listed by path: {out}"
    );
    assert!(
        !out.contains("never truncated into view"),
        "omitted page bodies must not leak: {out}"
    );
    assert!(
        out.contains("Recently updated pages") && out.contains("concepts/q.md"),
        "recent pointers survive the budget cut: {out}"
    );

    // Multi-byte safety: a body of 4-byte chars must cut on a boundary.
    let emoji_core = vec![ai_memory_store::BriefPageBody {
        path: "_rules/e.md".into(),
        title: "e".into(),
        body: "🦀".repeat(1_000),
        pinned: false,
        updated_at: "2026-07-12T00:00:00Z".into(),
    }];
    let out = render_session_brief(&emoji_core, &[], BRIEF_BUDGET_MIN).unwrap();
    assert!(out.is_char_boundary(out.len()), "must remain valid UTF-8");

    assert!(
        render_session_brief(&[], &[], BRIEF_BUDGET_DEFAULT).is_none(),
        "empty project must inject nothing"
    );
}

/// `max_chars` is what the operator sets to bound what every opted-in
/// session start costs, so the rendered brief must actually fit inside
/// it — including every section that renders *after* the page bodies
/// are spent, and after the escape pass has lengthened the text.
#[test]
fn render_session_brief_never_exceeds_budget() {
    // Worst case: more core pages than any budget can hold (so every
    // one of them costs an "omitted" line), a full recent list, and a
    // body carrying the untrusted-history marker, which the escape pass
    // lengthens by 6 chars per occurrence.
    let core: Vec<ai_memory_store::BriefPageBody> = (0..BRIEF_CORE_PAGES_LIMIT)
        .map(|i| ai_memory_store::BriefPageBody {
            path: format!("_rules/a-realistically-long-page-name-{i}.md"),
            title: format!("Rule number {i}"),
            body: format!(
                "{}{}",
                UNTRUSTED_HISTORY_START.repeat(40),
                "y".repeat(BRIEF_BUDGET_MAX),
            ),
            pinned: i == 0,
            updated_at: "2026-07-12T00:00:00Z".into(),
        })
        .collect();
    let recent: Vec<ai_memory_store::BriefingPage> = (0..BRIEF_RECENT_PAGES_LIMIT)
        .map(|i| ai_memory_store::BriefingPage {
            path: format!("concepts/a-recently-updated-page-{i}.md"),
            title: format!("A recently updated page titled {i}"),
            kind: "fact".into(),
            updated_at: "2026-07-12T00:00:00Z".into(),
        })
        .collect();

    for budget in [BRIEF_BUDGET_MIN, BRIEF_BUDGET_DEFAULT, BRIEF_BUDGET_MAX] {
        let out = render_session_brief(&core, &recent, budget).unwrap();
        assert!(
            out.len() <= budget,
            "brief must fit `max_chars`: budget={budget}, rendered={}",
            out.len(),
        );
        // The budget must never be balanced by dropping the boundary
        // that marks this text as untrusted.
        assert!(
            out.contains(ai_memory_core::UNTRUSTED_MEMORY_NOTICE)
                && out.contains(UNTRUSTED_HISTORY_START)
                && out.contains(UNTRUSTED_HISTORY_END),
            "the security boundary survives the budget: {out}"
        );
        assert!(out.is_char_boundary(out.len()), "must remain valid UTF-8");
    }
}
