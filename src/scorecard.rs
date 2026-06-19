use anyhow::{Context, Result};
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const RESET: &str = crate::theme::RESET;
const DIM: &str = crate::theme::MUTED;
const GREEN: &str = crate::theme::SUCCESS;
const YELLOW: &str = crate::theme::WARN;
const CYAN: &str = "\x1b[36m";
const BRIGHT_GREEN: &str = "\x1b[92m";
const STORE_DIR_ENV: &str = "SMALL_HARNESS_SCORECARD_DIR";
#[cfg(test)]
const DEFAULT_QUALITY_PR_THRESHOLD: u8 = 80;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkUnit {
    pub repo: String,
    pub branch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum ScorecardEvent {
    Turn(ScorecardTurn),
    Pr(ScorecardPr),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardTurn {
    pub timestamp: String,
    pub repo: String,
    pub branch: String,
    pub session_id: String,
    pub session_path: String,
    pub backend: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardPr {
    pub timestamp: String,
    pub repo: String,
    pub branch: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub status: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turn_count: usize,
    pub session_count: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ship_record_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality: Option<ScorecardQuality>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub session_traces: Vec<crate::turn_trace::SessionTraceSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScorecardQuality {
    pub score: u8,
    pub grade: String,
    pub counts: bool,
    pub status: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrQualityReadiness {
    Ready,
    NeedsReview,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrQualityTestStatus {
    Passed,
    Failed,
    NotRun,
}

#[derive(Debug, Clone)]
pub struct PrQualityInput {
    pub readiness: PrQualityReadiness,
    pub blockers: Vec<String>,
    pub warnings: Vec<String>,
    pub tests: PrQualityTestStatus,
    pub opened_by_gh: bool,
    pub has_pr_url: bool,
}

pub struct TurnRecordInput<'a> {
    pub workspace_root: &'a str,
    pub session_path: &'a Path,
    pub backend: &'a str,
    pub model: &'a str,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub enabled: bool,
}

pub struct PrCloseInput<'a> {
    pub workspace_root: &'a str,
    pub session_dir: &'a str,
    pub title: &'a str,
    pub url: Option<&'a str>,
    pub status: &'a str,
    pub quality: Option<PrQualityInput>,
    pub ship_record_path: Option<&'a str>,
    pub quality_threshold: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrCloseSummary {
    pub path: PathBuf,
    pub pr: ScorecardPr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenUnitSummary {
    pub repo: String,
    pub branch: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub turn_count: usize,
    pub session_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyScore {
    pub date: NaiveDate,
    pub pr_count: usize,
    pub scored_count: usize,
    pub quality_count: usize,
    pub total_quality_score: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone)]
pub struct ScorecardReport {
    pub path: Option<PathBuf>,
    pub lifetime_tokens: u64,
    pub turn_count: usize,
    pub closed_prs: Vec<ScorecardPr>,
    pub current_open: OpenUnitSummary,
    pub daily_scores: BTreeMap<NaiveDate, DailyScore>,
    pub current_streak: usize,
    pub longest_streak: usize,
    pub today: NaiveDate,
}

pub fn scorecard_path() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var(STORE_DIR_ENV) {
        return Some(PathBuf::from(dir).join("events.jsonl"));
    }
    let base = if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
        PathBuf::from(xdg)
    } else if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local").join("share")
    } else {
        return None;
    };
    Some(
        base.join("small-harness")
            .join("scorecard")
            .join("events.jsonl"),
    )
}

pub fn record_turn(input: TurnRecordInput<'_>) -> Result<Option<PathBuf>> {
    if !input.enabled {
        return Ok(None);
    }
    if input.input_tokens == 0 && input.output_tokens == 0 {
        return Ok(None);
    }
    let Some(path) = scorecard_path() else {
        return Ok(None);
    };
    let unit = current_work_unit(input.workspace_root);
    let session_id = input
        .session_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("session")
        .to_string();
    let session_path = session_path_for_storage(input.workspace_root, input.session_path);
    let event = ScorecardEvent::Turn(ScorecardTurn {
        timestamp: Utc::now().to_rfc3339(),
        repo: unit.repo,
        branch: unit.branch,
        session_id,
        session_path,
        backend: input.backend.to_string(),
        model: input.model.to_string(),
        input_tokens: input.input_tokens as u64,
        output_tokens: input.output_tokens as u64,
        total_tokens: input.input_tokens as u64 + input.output_tokens as u64,
    });
    append_event(&path, &event)?;
    Ok(Some(path))
}

pub fn close_pr_for_workspace(input: PrCloseInput<'_>) -> Result<Option<PrCloseSummary>> {
    let Some(path) = scorecard_path() else {
        return Ok(None);
    };
    close_pr_at_path(&path, input, Utc::now())
}

pub fn load_report(workspace_root: &str) -> Result<ScorecardReport> {
    let path = scorecard_path();
    let events = match &path {
        Some(path) => read_events(path)?,
        None => Vec::new(),
    };
    Ok(build_report(
        path,
        &events,
        current_work_unit(workspace_root),
        Utc::now().date_naive(),
    ))
}

pub fn recent_prs(limit: usize) -> Result<Vec<ScorecardPr>> {
    let Some(path) = scorecard_path() else {
        return Ok(Vec::new());
    };
    let mut prs: Vec<ScorecardPr> = read_events(&path)?
        .into_iter()
        .filter_map(|event| match event {
            ScorecardEvent::Pr(pr) => Some(pr),
            ScorecardEvent::Turn(_) => None,
        })
        .collect();
    prs.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    prs.truncate(limit);
    Ok(prs)
}

pub fn recent_pr_by_index(index: usize) -> Result<Option<ScorecardPr>> {
    if index == 0 {
        return Ok(None);
    }
    let prs = recent_prs(index)?;
    Ok(prs.get(index - 1).cloned())
}

pub fn reset_store() -> Result<Option<PathBuf>> {
    let Some(path) = scorecard_path() else {
        return Ok(None);
    };
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    }
    Ok(Some(path))
}

pub fn render_report(report: &ScorecardReport) -> String {
    let mut out = String::new();
    let total_prs = report.closed_prs.len();
    let quality_prs = quality_pr_count(&report.closed_prs);
    let scored_prs = scored_pr_count(&report.closed_prs);
    let needs_followup = scored_prs.saturating_sub(quality_prs);
    let clean_ships = clean_ship_count(&report.closed_prs);
    let avg_quality = average_quality_score(&report.closed_prs);
    let tokens_per_quality_pr = average_tokens_per_quality_pr(&report.closed_prs);
    let top_quality = top_quality_pr(&report.closed_prs);

    out.push_str(&format!(
        "  {DIM}scorecard{RESET}       global quality PRs shipped\n"
    ));
    match &report.path {
        Some(path) => out.push_str(&format!(
            "  {DIM}store{RESET}           {}\n",
            path.display()
        )),
        None => out.push_str(&format!("  {DIM}store{RESET}           unavailable\n")),
    }
    out.push('\n');
    out.push_str(&format!(
        "  {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}\n",
        "Quality PRs",
        format!("{quality_prs}/{total_prs}"),
        "Quality rate",
        format_rate(quality_prs, total_prs),
        "Avg quality",
        avg_quality
            .map(format_quality_score)
            .unwrap_or_else(|| "n/a".to_string())
    ));
    out.push_str(&format!(
        "  {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}\n",
        "Clean ships",
        clean_ships,
        "Needs follow",
        needs_followup,
        "Tokens / QPR",
        tokens_per_quality_pr
            .map(format_tokens)
            .unwrap_or_else(|| "n/a".to_string())
    ));
    out.push_str(&format!(
        "  {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}\n",
        "Tracked",
        format_tokens(report.lifetime_tokens),
        "Streak",
        format!("{}d / {}d", report.current_streak, report.longest_streak),
        "Scored PRs",
        scored_prs
    ));
    out.push_str(&format!(
        "  {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}   {CYAN}{:<13}{RESET} {:>10}\n",
        "Open branch",
        format_tokens(report.current_open.total_tokens),
        "Open turns",
        report.current_open.turn_count,
        "All turns",
        report.turn_count
    ));
    out.push('\n');
    out.push_str(&format!(
        "  {DIM}current{RESET}         {} · {}\n",
        repo_label(&report.current_open.repo),
        report.current_open.branch
    ));
    if let Some((pr, quality)) = top_quality {
        out.push_str(&format!(
            "  {DIM}top quality{RESET}     {} {} · {} · {}\n",
            quality.grade,
            format_quality_score(quality.score),
            pr_date_label(&pr.timestamp),
            one_line(&pr.title, 64)
        ));
    }
    out.push('\n');
    out.push_str(&render_daily_grid(report));
    if total_prs == 0 {
        out.push_str(&format!(
            "\n  {DIM}No closed PR units yet. Run /scorecard close <label> or /ship pr after doing tracked work.{RESET}\n"
        ));
    } else {
        out.push_str(&format!(
            "\n  {DIM}Daily grid shows quality PRs shipped. Tokens are context, not the score.{RESET}\n"
        ));
        out.push_str(&format!("  {DIM}recent detail → /scorecard pr 1{RESET}\n"));
    }
    out
}

pub fn render_recent_prs(prs: &[ScorecardPr]) -> String {
    let mut out = String::new();
    out.push_str(&format!("  {DIM}scorecard PRs{RESET}\n"));
    if prs.is_empty() {
        out.push_str(&format!(
            "  {DIM}No closed PR units yet. Run /scorecard close <label> or /ship pr.{RESET}\n"
        ));
        return out;
    }
    for (index, pr) in prs.iter().enumerate() {
        let url = pr
            .url
            .as_deref()
            .map(|url| format!(" · {url}"))
            .unwrap_or_default();
        let quality = format_quality_cell(pr.quality.as_ref());
        out.push_str(&format!(
            "  #{:<3} {}  {:<18} {:>8}  {} · {} · {}{}\n",
            index + 1,
            pr_date_label(&pr.timestamp),
            quality,
            format_tokens(pr.total_tokens),
            repo_label(&pr.repo),
            pr.branch,
            one_line(&pr.title, 64),
            url
        ));
        let evidence = render_pr_evidence_line(pr);
        if !evidence.is_empty() {
            out.push_str(&format!("        {DIM}{evidence}{RESET}\n"));
        }
    }
    out.push_str(&format!("\n  {DIM}detail → /scorecard pr <n>{RESET}\n"));
    out
}

pub fn render_pr_detail(pr: &ScorecardPr, index: usize) -> String {
    let mut out = String::new();
    out.push_str(&format!("  {DIM}scorecard PR{RESET}     #{index}\n"));
    out.push_str(&format!(
        "  {DIM}title{RESET}           {}\n",
        one_line(&pr.title, 120)
    ));
    out.push_str(&format!(
        "  {DIM}closed{RESET}          {} · {} · {}\n",
        pr_date_label(&pr.timestamp),
        repo_label(&pr.repo),
        pr.branch
    ));
    out.push_str(&format!(
        "  {DIM}status{RESET}          {} · {} tokens · {} turn(s) · {} session(s)\n",
        pr.status,
        format_tokens(pr.total_tokens),
        pr.turn_count,
        pr.session_count
    ));
    if let Some(url) = pr.url.as_deref() {
        out.push_str(&format!("  {DIM}url{RESET}             {url}\n"));
    }
    if let Some(quality) = pr.quality.as_ref() {
        out.push_str(&format!(
            "  {DIM}quality{RESET}         {} {} · {}\n",
            quality.grade,
            format_quality_score(quality.score),
            quality.status
        ));
        if !quality.evidence.is_empty() {
            out.push_str(&format!("  {DIM}evidence{RESET}\n"));
            for item in &quality.evidence {
                out.push_str(&format!("    {GREEN}·{RESET} {item}\n"));
            }
        }
        if !quality.blockers.is_empty() {
            out.push_str(&format!("  {DIM}blockers{RESET}\n"));
            for item in &quality.blockers {
                out.push_str(&format!("    {YELLOW}!{RESET} {item}\n"));
            }
        }
        if !quality.warnings.is_empty() {
            out.push_str(&format!("  {DIM}warnings{RESET}\n"));
            for item in &quality.warnings {
                out.push_str(&format!("    {YELLOW}!{RESET} {item}\n"));
            }
        }
    } else {
        out.push_str(&format!("  {DIM}quality{RESET}         unrated\n"));
    }
    if let Some(path) = pr.ship_record_path.as_deref() {
        out.push_str(&format!("  {DIM}ship record{RESET}    {path}\n"));
    }
    out.push('\n');
    if pr.session_traces.is_empty() && pr.session_ids.is_empty() {
        out.push_str(&format!(
            "  {DIM}sessions{RESET}        no session audit captured at close\n"
        ));
    } else if !pr.session_traces.is_empty() {
        out.push_str(&format!("  {DIM}sessions{RESET}\n"));
        for trace in &pr.session_traces {
            out.push_str(&format!("    {CYAN}{}{RESET}\n", trace.session_id));
            if trace.trace_found {
                out.push_str(&format!(
                    "      {} turn(s) · {} step(s) · {} tool(s) · {} subagent(s) · {} approval(s)\n",
                    trace.turn_count,
                    trace.total_steps,
                    trace.tool_calls,
                    trace.subagent_runs,
                    trace.approvals
                ));
                out.push_str(&format!(
                    "      model {:.1}s · tools {:.1}s · approval {:.1}s · total {:.1}s\n",
                    trace.model_ms as f64 / 1000.0,
                    trace.tool_ms as f64 / 1000.0,
                    trace.approval_ms as f64 / 1000.0,
                    trace.total_ms as f64 / 1000.0
                ));
                out.push_str(&format!(
                    "      {DIM}events → {}{RESET}\n",
                    trace.events_path
                ));
            } else {
                out.push_str(&format!(
                    "      {DIM}no event log at close (display.eventLog.enabled may have been off){RESET}\n"
                ));
            }
        }
    } else {
        out.push_str(&format!(
            "  {DIM}sessions{RESET}        {}\n",
            pr.session_ids.join(", ")
        ));
        for path in &pr.session_paths {
            out.push_str(&format!("    {DIM}session → {path}{RESET}\n"));
        }
    }
    out.push_str(&format!(
        "\n  {DIM}raw trace export → /export <session> events{RESET}\n"
    ));
    out
}

fn render_pr_evidence_line(pr: &ScorecardPr) -> String {
    let mut parts = Vec::new();
    if !pr.session_ids.is_empty() {
        parts.push(format!("{} session(s)", pr.session_ids.len()));
    }
    if pr.ship_record_path.is_some() {
        parts.push("ship record".into());
    }
    if let Some(quality) = pr.quality.as_ref() {
        if !quality.evidence.is_empty() {
            parts.push(quality.evidence.join(" · "));
        }
    }
    parts.join(" · ")
}

pub fn render_current(summary: &OpenUnitSummary) -> String {
    format!(
        "  {DIM}current PR unit{RESET} {}\n  {DIM}branch{RESET}          {}\n  {DIM}tokens{RESET}          {} total · {} in · {} out\n  {DIM}turns{RESET}           {} turn(s) · {} session(s)\n  {DIM}quality{RESET}         scored when /ship pr closes with readiness/test evidence\n",
        repo_label(&summary.repo),
        summary.branch,
        format_tokens(summary.total_tokens),
        format_tokens(summary.input_tokens),
        format_tokens(summary.output_tokens),
        summary.turn_count,
        summary.session_count
    )
}

pub fn current_summary(workspace_root: &str) -> Result<OpenUnitSummary> {
    let events = match scorecard_path() {
        Some(path) => read_events(&path)?,
        None => Vec::new(),
    };
    Ok(open_summary_for_unit(
        &events,
        &current_work_unit(workspace_root),
    ))
}

fn close_pr_at_path(
    path: &Path,
    input: PrCloseInput<'_>,
    now: DateTime<Utc>,
) -> Result<Option<PrCloseSummary>> {
    let events = read_events(path)?;
    let unit = current_work_unit(input.workspace_root);
    let open = open_summary_for_unit(&events, &unit);
    if open.turn_count == 0 {
        return Ok(None);
    }
    let open_sessions = open_sessions_for_unit(&events, &unit);
    let session_traces =
        collect_session_traces(&open_sessions, input.workspace_root, input.session_dir);
    let pr = ScorecardPr {
        timestamp: now.to_rfc3339(),
        repo: unit.repo,
        branch: unit.branch,
        title: normalize_label(input.title),
        url: input
            .url
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .map(str::to_string),
        status: input.status.trim().to_string(),
        input_tokens: open.input_tokens,
        output_tokens: open.output_tokens,
        total_tokens: open.total_tokens,
        turn_count: open.turn_count,
        session_count: open.session_count,
        quality: input
            .quality
            .map(|quality| assess_pr_quality(quality, input.quality_threshold)),
        session_ids: open_sessions.session_ids,
        session_paths: open_sessions.session_paths,
        ship_record_path: input
            .ship_record_path
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_string),
        session_traces,
    };
    append_event(path, &ScorecardEvent::Pr(pr.clone()))?;
    Ok(Some(PrCloseSummary {
        path: path.to_path_buf(),
        pr,
    }))
}

fn build_report(
    path: Option<PathBuf>,
    events: &[ScorecardEvent],
    current_unit: WorkUnit,
    today: NaiveDate,
) -> ScorecardReport {
    let mut lifetime_tokens = 0_u64;
    let mut turn_count = 0_usize;
    let mut closed_prs = Vec::new();
    let mut daily_scores: BTreeMap<NaiveDate, DailyScore> = BTreeMap::new();

    for event in events {
        match event {
            ScorecardEvent::Turn(turn) => {
                lifetime_tokens += turn.total_tokens;
                turn_count += 1;
            }
            ScorecardEvent::Pr(pr) => {
                if let Some(date) = date_from_timestamp(&pr.timestamp) {
                    let score = daily_scores.entry(date).or_insert(DailyScore {
                        date,
                        pr_count: 0,
                        scored_count: 0,
                        quality_count: 0,
                        total_quality_score: 0,
                        total_tokens: 0,
                    });
                    score.pr_count += 1;
                    score.total_tokens += pr.total_tokens;
                    if let Some(quality) = &pr.quality {
                        score.scored_count += 1;
                        score.total_quality_score += quality.score as u64;
                        if quality.counts {
                            score.quality_count += 1;
                        }
                    }
                }
                closed_prs.push(pr.clone());
            }
        }
    }

    closed_prs.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    let (current_streak, longest_streak) = streaks(&daily_scores, today);
    let current_open = open_summary_for_unit(events, &current_unit);

    ScorecardReport {
        path,
        lifetime_tokens,
        turn_count,
        closed_prs,
        current_open,
        daily_scores,
        current_streak,
        longest_streak,
        today,
    }
}

fn open_summary_for_unit(events: &[ScorecardEvent], unit: &WorkUnit) -> OpenUnitSummary {
    let latest_close = latest_pr_timestamp(events, unit);
    let mut input_tokens = 0_u64;
    let mut output_tokens = 0_u64;
    let mut total_tokens = 0_u64;
    let mut turn_count = 0_usize;
    let mut sessions = BTreeSet::new();

    for event in events {
        let ScorecardEvent::Turn(turn) = event else {
            continue;
        };
        if turn.repo != unit.repo || turn.branch != unit.branch {
            continue;
        }
        if !is_after(&turn.timestamp, latest_close.as_ref()) {
            continue;
        }
        input_tokens += turn.input_tokens;
        output_tokens += turn.output_tokens;
        total_tokens += turn.total_tokens;
        turn_count += 1;
        sessions.insert(turn.session_id.clone());
    }

    OpenUnitSummary {
        repo: unit.repo.clone(),
        branch: unit.branch.clone(),
        input_tokens,
        output_tokens,
        total_tokens,
        turn_count,
        session_count: sessions.len(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionRef {
    session_id: String,
    session_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OpenUnitSessions {
    sessions: Vec<SessionRef>,
    session_ids: Vec<String>,
    session_paths: Vec<String>,
}

fn open_sessions_for_unit(events: &[ScorecardEvent], unit: &WorkUnit) -> OpenUnitSessions {
    let latest_close = latest_pr_timestamp(events, unit);
    let mut sessions = BTreeMap::new();

    for event in events {
        let ScorecardEvent::Turn(turn) = event else {
            continue;
        };
        if turn.repo != unit.repo || turn.branch != unit.branch {
            continue;
        }
        if !is_after(&turn.timestamp, latest_close.as_ref()) {
            continue;
        }
        sessions
            .entry(turn.session_id.clone())
            .or_insert_with(|| turn.session_path.clone());
    }

    let sessions: Vec<SessionRef> = sessions
        .into_iter()
        .map(|(session_id, session_path)| SessionRef {
            session_id,
            session_path,
        })
        .collect();
    let session_ids: Vec<String> = sessions.iter().map(|s| s.session_id.clone()).collect();
    let session_paths: Vec<String> = sessions.iter().map(|s| s.session_path.clone()).collect();

    OpenUnitSessions {
        sessions,
        session_ids,
        session_paths,
    }
}

fn collect_session_traces(
    open_sessions: &OpenUnitSessions,
    workspace_root: &str,
    session_dir: &str,
) -> Vec<crate::turn_trace::SessionTraceSummary> {
    open_sessions
        .sessions
        .iter()
        .map(|session| {
            let resolved = crate::turn_trace::resolve_session_path(
                &session.session_path,
                workspace_root,
                session_dir,
                &session.session_id,
            );
            let mut summary = crate::turn_trace::summarize_session_trace(&resolved);
            summary.session_id = session.session_id.clone();
            summary.events_path = crate::turn_trace::events_path_for_session(&resolved)
                .display()
                .to_string();
            summary
        })
        .collect()
}

fn session_path_for_storage(workspace_root: &str, session_path: &Path) -> String {
    let workspace = Path::new(workspace_root);
    session_path
        .strip_prefix(workspace)
        .map(|path| path.display().to_string())
        .unwrap_or_else(|_| session_path.display().to_string())
}

pub fn format_scorecard_suffix(
    workspace_root: &str,
    enabled: bool,
    nudge_min_turns: usize,
) -> Result<String> {
    if !enabled {
        return Ok(String::new());
    }
    let summary = current_summary(workspace_root)?;
    if summary.turn_count == 0 || summary.turn_count < nudge_min_turns {
        return Ok(String::new());
    }
    if summary.branch == "main" || summary.branch == "master" {
        return Ok(String::new());
    }
    Ok(format!(
        " · {} turn(s) tracked · /ship pr closes scorecard",
        summary.turn_count
    ))
}

fn latest_pr_timestamp(events: &[ScorecardEvent], unit: &WorkUnit) -> Option<DateTime<Utc>> {
    events
        .iter()
        .filter_map(|event| match event {
            ScorecardEvent::Pr(pr) if pr.repo == unit.repo && pr.branch == unit.branch => {
                parse_timestamp(&pr.timestamp)
            }
            _ => None,
        })
        .max()
}

fn is_after(timestamp: &str, after: Option<&DateTime<Utc>>) -> bool {
    match after {
        Some(after) => parse_timestamp(timestamp)
            .map(|timestamp| timestamp > *after)
            .unwrap_or(false),
        None => true,
    }
}

fn assess_pr_quality(input: PrQualityInput, quality_threshold: u8) -> ScorecardQuality {
    let mut score = 100_i16;
    let mut evidence = Vec::new();

    match input.readiness {
        PrQualityReadiness::Ready => evidence.push("ship readiness ready".to_string()),
        PrQualityReadiness::NeedsReview => {
            score -= 10;
            evidence.push("ship readiness needs review".to_string());
        }
        PrQualityReadiness::Blocked => {
            score -= 45;
            evidence.push("ship readiness blocked".to_string());
        }
    }

    match input.tests {
        PrQualityTestStatus::Passed => evidence.push("tests passed".to_string()),
        PrQualityTestStatus::Failed => {
            score -= 40;
            evidence.push("tests failed".to_string());
        }
        PrQualityTestStatus::NotRun => {
            score -= 20;
            evidence.push("tests not run".to_string());
        }
    }

    if input.opened_by_gh {
        evidence.push("PR command succeeded".to_string());
    } else {
        score -= 15;
        evidence.push("PR command not verified".to_string());
    }
    if input.has_pr_url {
        evidence.push("PR URL captured".to_string());
    } else {
        score -= 5;
        evidence.push("PR URL not captured".to_string());
    }

    score -= (input.blockers.len() as i16 * 15).min(45);
    score -= (input.warnings.len() as i16 * 5).min(20);

    let score = score.clamp(0, 100) as u8;
    let has_pr_evidence = input.opened_by_gh || input.has_pr_url;
    let counts = score >= quality_threshold
        && input.readiness != PrQualityReadiness::Blocked
        && input.tests == PrQualityTestStatus::Passed
        && has_pr_evidence;
    let status = if counts {
        "quality"
    } else if input.readiness == PrQualityReadiness::Blocked {
        "blocked"
    } else {
        "needsReview"
    };

    ScorecardQuality {
        score,
        grade: grade_for_score(score).to_string(),
        counts,
        status: status.to_string(),
        evidence,
        blockers: input.blockers,
        warnings: input.warnings,
    }
}

fn append_event(path: &Path, event: &ScorecardEvent) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let json = serde_json::to_string(event)?;
    let mut f = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    f.write_all(json.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

fn read_events(path: &Path) -> Result<Vec<ScorecardEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let reader = BufReader::new(file);
    let mut events = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<ScorecardEvent>(&line) {
            events.push(event);
        }
    }
    Ok(events)
}

fn current_work_unit(workspace_root: &str) -> WorkUnit {
    let repo = git_capture(workspace_root, &["rev-parse", "--show-toplevel"])
        .map(|value| canonical_or_display(Path::new(value.trim())))
        .unwrap_or_else(|_| canonical_or_display(Path::new(workspace_root)));
    let branch = git_capture(workspace_root, &["rev-parse", "--abbrev-ref", "HEAD"])
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(|value| {
            if value == "HEAD" {
                git_capture(workspace_root, &["rev-parse", "--short", "HEAD"])
                    .map(|hash| format!("detached:{}", hash.trim()))
                    .unwrap_or_else(|_| "detached".into())
            } else {
                value
            }
        })
        .unwrap_or_else(|| "(no-git)".into());
    WorkUnit { repo, branch }
}

fn git_capture(workspace_root: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(workspace_root)
        .args(args)
        .output()
        .with_context(|| format!("running git {}", args.join(" ")))?;
    if !output.status.success() {
        anyhow::bail!("git {} failed", args.join(" "));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn canonical_or_display(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn render_daily_grid(report: &ScorecardReport) -> String {
    let weeks = 53_i64;
    let raw_start = report.today - Duration::days(weeks * 7 - 1);
    let start = raw_start - Duration::days(raw_start.weekday().num_days_from_sunday() as i64);
    let mut out = String::new();
    out.push_str(&format!("  {DIM}Quality PR activity{RESET}\n"));
    out.push_str(&render_month_labels(start, weeks as usize));
    let labels = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    for day in 0..7_i64 {
        out.push_str(&format!("  {:<3}", labels[day as usize]));
        for week in 0..weeks {
            let date = start + Duration::days(week * 7 + day);
            if date > report.today {
                out.push_str("  ");
                continue;
            }
            out.push_str(&quality_square(report.daily_scores.get(&date)));
            out.push(' ');
        }
        out.push('\n');
    }
    out.push_str(&format!(
        "      {DIM}empty{RESET} {}  {BRIGHT_GREEN}all quality{RESET} {GREEN}some quality{RESET} {YELLOW}needs review / unrated{RESET}\n",
        empty_square()
    ));
    out
}

fn render_month_labels(start: NaiveDate, weeks: usize) -> String {
    let mut cells = vec![' '; weeks * 2 + 4];
    let mut last_month = None;
    for week in 0..weeks {
        let date = start + Duration::days(week as i64 * 7);
        let month = date.month();
        if last_month != Some(month) {
            let label = month_label(month);
            let pos = 4 + week * 2;
            for (idx, ch) in label.chars().enumerate() {
                if pos + idx < cells.len() {
                    cells[pos + idx] = ch;
                }
            }
            last_month = Some(month);
        }
    }
    let mut out = String::from("  ");
    out.extend(cells);
    out.push('\n');
    out
}

fn quality_square(score: Option<&DailyScore>) -> String {
    let Some(score) = score else {
        return empty_square();
    };
    if score.pr_count == 0 {
        return empty_square();
    }
    let color = if score.quality_count > 0 && score.quality_count == score.pr_count {
        BRIGHT_GREEN
    } else if score.quality_count > 0 {
        GREEN
    } else {
        YELLOW
    };
    format!("{color}■{RESET}")
}

fn empty_square() -> String {
    format!("{DIM}□{RESET}")
}

fn streaks(scores: &BTreeMap<NaiveDate, DailyScore>, today: NaiveDate) -> (usize, usize) {
    let active: BTreeSet<NaiveDate> = scores
        .iter()
        .filter_map(|(date, score)| (score.quality_count > 0).then_some(*date))
        .collect();
    if active.is_empty() {
        return (0, 0);
    }

    let mut current = 0_usize;
    let mut cursor = today;
    while active.contains(&cursor) {
        current += 1;
        cursor -= Duration::days(1);
    }

    let mut longest = 0_usize;
    let mut run = 0_usize;
    let first = *active.iter().next().unwrap();
    let last = *active.iter().next_back().unwrap();
    let mut day = first;
    while day <= last {
        if active.contains(&day) {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
        day += Duration::days(1);
    }
    (current, longest)
}

fn scored_pr_count(prs: &[ScorecardPr]) -> usize {
    prs.iter().filter(|pr| pr.quality.is_some()).count()
}

fn quality_pr_count(prs: &[ScorecardPr]) -> usize {
    prs.iter()
        .filter(|pr| pr.quality.as_ref().is_some_and(|quality| quality.counts))
        .count()
}

fn clean_ship_count(prs: &[ScorecardPr]) -> usize {
    prs.iter()
        .filter(|pr| {
            pr.quality
                .as_ref()
                .is_some_and(|quality| quality.counts && quality.score >= 90)
        })
        .count()
}

fn average_quality_score(prs: &[ScorecardPr]) -> Option<u8> {
    let mut count = 0_u64;
    let mut total = 0_u64;
    for quality in prs.iter().filter_map(|pr| pr.quality.as_ref()) {
        count += 1;
        total += quality.score as u64;
    }
    (count > 0).then_some((total / count) as u8)
}

fn average_tokens_per_quality_pr(prs: &[ScorecardPr]) -> Option<u64> {
    let mut count = 0_u64;
    let mut total = 0_u64;
    for pr in prs
        .iter()
        .filter(|pr| pr.quality.as_ref().is_some_and(|quality| quality.counts))
    {
        count += 1;
        total += pr.total_tokens;
    }
    (count > 0).then_some(total / count)
}

fn top_quality_pr(prs: &[ScorecardPr]) -> Option<(&ScorecardPr, &ScorecardQuality)> {
    prs.iter()
        .filter_map(|pr| pr.quality.as_ref().map(|quality| (pr, quality)))
        .max_by_key(|(_, quality)| quality.score)
}

fn format_quality_cell(quality: Option<&ScorecardQuality>) -> String {
    match quality {
        Some(quality) => format!(
            "{} {:>3} {}",
            quality.grade,
            format_quality_score(quality.score),
            quality.status
        ),
        None => "--  n/a unrated".to_string(),
    }
}

fn format_quality_score(score: u8) -> String {
    format!("{score}/100")
}

fn format_rate(numerator: usize, denominator: usize) -> String {
    if denominator == 0 {
        "n/a".to_string()
    } else {
        format!("{:.0}%", numerator as f64 * 100.0 / denominator as f64)
    }
}

fn grade_for_score(score: u8) -> &'static str {
    match score {
        90..=100 => "A",
        80..=89 => "B",
        70..=79 => "C",
        60..=69 => "D",
        _ => "F",
    }
}

fn parse_timestamp(timestamp: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

fn date_from_timestamp(timestamp: &str) -> Option<NaiveDate> {
    parse_timestamp(timestamp).map(|dt| dt.date_naive())
}

fn pr_date_label(timestamp: &str) -> String {
    date_from_timestamp(timestamp)
        .map(|date| date.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "unknown-date".into())
}

fn normalize_label(label: &str) -> String {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        "Untitled PR".into()
    } else {
        trimmed.to_string()
    }
}

fn repo_label(repo: &str) -> String {
    Path::new(repo)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(repo)
        .to_string()
}

fn one_line(text: &str, max_chars: usize) -> String {
    let mut out = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.chars().count() <= max_chars {
        return out;
    }
    let mut truncated = String::new();
    for ch in out.chars().take(max_chars.saturating_sub(1)) {
        truncated.push(ch);
    }
    truncated.push('…');
    out = truncated;
    out
}

fn month_label(month: u32) -> &'static str {
    match month {
        1 => "Jan",
        2 => "Feb",
        3 => "Mar",
        4 => "Apr",
        5 => "May",
        6 => "Jun",
        7 => "Jul",
        8 => "Aug",
        9 => "Sep",
        10 => "Oct",
        11 => "Nov",
        12 => "Dec",
        _ => "",
    }
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;

    fn ts(day: u32) -> String {
        format!("2026-06-{day:02}T12:00:00Z")
    }

    fn turn(day: u32, tokens: u64, session: &str) -> ScorecardEvent {
        ScorecardEvent::Turn(ScorecardTurn {
            timestamp: ts(day),
            repo: "/repo/demo".into(),
            branch: "feature".into(),
            session_id: session.into(),
            session_path: format!(".sessions/{session}.jsonl"),
            backend: "openrouter".into(),
            model: "model".into(),
            input_tokens: tokens / 2,
            output_tokens: tokens - tokens / 2,
            total_tokens: tokens,
        })
    }

    fn pr(day: u32, tokens: u64, title: &str) -> ScorecardEvent {
        ScorecardEvent::Pr(ScorecardPr {
            timestamp: ts(day),
            repo: "/repo/demo".into(),
            branch: "feature".into(),
            title: title.into(),
            url: None,
            status: "created".into(),
            input_tokens: tokens,
            output_tokens: 0,
            total_tokens: tokens,
            turn_count: 1,
            session_count: 1,
            session_ids: Vec::new(),
            session_paths: Vec::new(),
            ship_record_path: None,
            quality: None,
            session_traces: Vec::new(),
        })
    }

    fn quality_pr(day: u32, tokens: u64, title: &str, score: u8, counts: bool) -> ScorecardEvent {
        ScorecardEvent::Pr(ScorecardPr {
            timestamp: ts(day),
            repo: "/repo/demo".into(),
            branch: "feature".into(),
            title: title.into(),
            url: None,
            status: "created".into(),
            input_tokens: tokens,
            output_tokens: 0,
            total_tokens: tokens,
            turn_count: 1,
            session_count: 1,
            session_ids: Vec::new(),
            session_paths: Vec::new(),
            ship_record_path: None,
            quality: Some(ScorecardQuality {
                score,
                grade: grade_for_score(score).into(),
                counts,
                status: if counts { "quality" } else { "needsReview" }.into(),
                evidence: vec!["tests passed".into()],
                blockers: Vec::new(),
                warnings: Vec::new(),
            }),
            session_traces: Vec::new(),
        })
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn report_uses_quality_score_instead_of_lowest_tokens() {
        let events = vec![
            turn(10, 1000, "a"),
            quality_pr(10, 1000, "well tested", 94, true),
            turn(11, 200, "b"),
            quality_pr(11, 200, "thin evidence", 72, false),
        ];
        let report = build_report(
            None,
            &events,
            WorkUnit {
                repo: "/repo/demo".into(),
                branch: "feature".into(),
            },
            NaiveDate::from_ymd_opt(2026, 6, 11).unwrap(),
        );
        let (top, quality) = top_quality_pr(&report.closed_prs).unwrap();
        assert_eq!(top.title, "well tested");
        assert_eq!(quality.score, 94);
        assert_eq!(quality_pr_count(&report.closed_prs), 1);
        assert_eq!(
            average_tokens_per_quality_pr(&report.closed_prs),
            Some(1000)
        );
        assert_eq!(report.longest_streak, 1);
    }

    #[test]
    fn open_summary_counts_only_turns_after_latest_pr_boundary() {
        let events = vec![
            turn(10, 1000, "a"),
            pr(10, 1000, "first"),
            turn(11, 300, "b"),
            turn(12, 700, "c"),
        ];
        let summary = open_summary_for_unit(
            &events,
            &WorkUnit {
                repo: "/repo/demo".into(),
                branch: "feature".into(),
            },
        );
        assert_eq!(summary.total_tokens, 1000);
        assert_eq!(summary.turn_count, 2);
        assert_eq!(summary.session_count, 2);
    }

    #[test]
    fn render_recent_prs_lists_raw_token_totals() {
        let prs = vec![match quality_pr(12, 42000, "tiny fix", 91, true) {
            ScorecardEvent::Pr(pr) => pr,
            _ => unreachable!(),
        }];
        let rendered = render_recent_prs(&prs);
        assert!(rendered.contains("#1  "));
        assert!(rendered.contains("A"));
        assert!(rendered.contains("91/100"));
        assert!(rendered.contains("42.0k"));
        assert!(rendered.contains("tiny fix"));
    }

    #[test]
    fn quality_assessment_requires_tests_and_pr_creation() {
        let warnings = vec!["tests were not run for this PR".to_string()];
        let quality = assess_pr_quality(
            PrQualityInput {
                readiness: PrQualityReadiness::NeedsReview,
                blockers: vec![],
                warnings,
                tests: PrQualityTestStatus::NotRun,
                opened_by_gh: true,
                has_pr_url: true,
            },
            DEFAULT_QUALITY_PR_THRESHOLD,
        );

        assert_eq!(quality.score, 65);
        assert_eq!(quality.grade, "D");
        assert!(!quality.counts);
        assert_eq!(quality.status, "needsReview");
    }

    #[test]
    fn close_pr_at_path_writes_boundary_for_current_branch() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init"]);
        git(repo.path(), &["checkout", "-b", "feature"]);
        let path = repo.path().join("events.jsonl");
        let unit = current_work_unit(repo.path().to_str().unwrap());

        append_event(
            &path,
            &ScorecardEvent::Turn(ScorecardTurn {
                timestamp: ts(13),
                repo: unit.repo.clone(),
                branch: unit.branch.clone(),
                session_id: "a".into(),
                session_path: ".sessions/a.jsonl".into(),
                backend: "openrouter".into(),
                model: "model".into(),
                input_tokens: 700,
                output_tokens: 300,
                total_tokens: 1000,
            }),
        )
        .unwrap();

        let closed = close_pr_at_path(
            &path,
            PrCloseInput {
                workspace_root: repo.path().to_str().unwrap(),
                session_dir: ".sessions",
                title: "feature PR",
                url: Some("https://example.com/pr/1"),
                status: "created",
                quality: Some(PrQualityInput {
                    readiness: PrQualityReadiness::Ready,
                    blockers: vec![],
                    warnings: vec![],
                    tests: PrQualityTestStatus::Passed,
                    opened_by_gh: true,
                    has_pr_url: true,
                }),
                ship_record_path: Some(".sessions/ship-pr.md"),
                quality_threshold: DEFAULT_QUALITY_PR_THRESHOLD,
            },
            DateTime::parse_from_rfc3339("2026-06-14T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap()
        .unwrap();

        assert_eq!(closed.pr.total_tokens, 1000);
        assert_eq!(closed.pr.turn_count, 1);
        assert_eq!(closed.pr.title, "feature PR");
        assert_eq!(closed.pr.url.as_deref(), Some("https://example.com/pr/1"));
        assert_eq!(closed.pr.quality.as_ref().unwrap().score, 100);
        assert!(closed.pr.quality.as_ref().unwrap().counts);
        assert_eq!(closed.pr.session_ids, vec!["a".to_string()]);
        assert_eq!(
            closed.pr.ship_record_path.as_deref(),
            Some(".sessions/ship-pr.md")
        );
        assert!(close_pr_at_path(
            &path,
            PrCloseInput {
                workspace_root: repo.path().to_str().unwrap(),
                session_dir: ".sessions",
                title: "duplicate",
                url: None,
                status: "manual",
                quality: None,
                ship_record_path: None,
                quality_threshold: DEFAULT_QUALITY_PR_THRESHOLD,
            },
            DateTime::parse_from_rfc3339("2026-06-14T13:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap()
        .is_none());
    }

    #[test]
    fn manual_quality_input_counts_with_url_and_tests() {
        let quality = assess_pr_quality(
            PrQualityInput {
                readiness: PrQualityReadiness::Ready,
                blockers: vec![],
                warnings: vec![],
                tests: PrQualityTestStatus::Passed,
                opened_by_gh: false,
                has_pr_url: true,
            },
            DEFAULT_QUALITY_PR_THRESHOLD,
        );
        assert_eq!(quality.score, 85);
        assert!(quality.counts);
    }

    #[test]
    fn render_pr_detail_includes_quality_and_sessions() {
        let pr = ScorecardPr {
            timestamp: ts(12),
            repo: "/repo/demo".into(),
            branch: "feature".into(),
            title: "audit me".into(),
            url: Some("https://example.com/pr/2".into()),
            status: "created".into(),
            input_tokens: 1000,
            output_tokens: 500,
            total_tokens: 1500,
            turn_count: 3,
            session_count: 1,
            session_ids: vec!["sess-a".into()],
            session_paths: vec![".sessions/sess-a.jsonl".into()],
            ship_record_path: Some(".sessions/ship-pr.md".into()),
            quality: Some(ScorecardQuality {
                score: 91,
                grade: "A".into(),
                counts: true,
                status: "quality".into(),
                evidence: vec!["tests passed".into()],
                blockers: Vec::new(),
                warnings: Vec::new(),
            }),
            session_traces: vec![crate::turn_trace::SessionTraceSummary {
                session_id: "sess-a".into(),
                events_path: ".sessions/sess-a.events.jsonl".into(),
                turn_count: 3,
                total_steps: 8,
                tool_calls: 4,
                subagent_runs: 1,
                approvals: 2,
                model_ms: 1000,
                tool_ms: 2000,
                approval_ms: 100,
                total_ms: 3200,
                trace_found: true,
            }],
        };
        let rendered = render_pr_detail(&pr, 1);
        assert!(rendered.contains("#1"));
        assert!(rendered.contains("audit me"));
        assert!(rendered.contains("A 91/100"));
        assert!(rendered.contains("sess-a"));
        assert!(rendered.contains("ship record"));
        assert!(rendered.contains("4 tool(s)"));
    }

    #[test]
    fn close_pr_attaches_session_traces_when_events_exist() {
        let repo = tempfile::tempdir().unwrap();
        git(repo.path(), &["init"]);
        git(repo.path(), &["checkout", "-b", "feature"]);
        let sessions_dir = repo.path().join(".sessions");
        std::fs::create_dir_all(&sessions_dir).unwrap();
        let session_path = sessions_dir.join("audit-a.jsonl");
        let mut trace = crate::turn_trace::TurnTrace::open(&session_path, true).unwrap();
        trace.begin_turn();
        trace
            .log_turn_summary(crate::turn_trace::TurnMetrics {
                steps: 2,
                ttft_ms: None,
                model_ms: 100,
                tool_ms: 200,
                approval_ms: 0,
                total_ms: 0,
                hit_step_limit: false,
            })
            .unwrap();

        let path = repo.path().join("events.jsonl");
        let unit = current_work_unit(repo.path().to_str().unwrap());
        append_event(
            &path,
            &ScorecardEvent::Turn(ScorecardTurn {
                timestamp: ts(13),
                repo: unit.repo.clone(),
                branch: unit.branch.clone(),
                session_id: "audit-a".into(),
                session_path: ".sessions/audit-a.jsonl".into(),
                backend: "openrouter".into(),
                model: "model".into(),
                input_tokens: 500,
                output_tokens: 500,
                total_tokens: 1000,
            }),
        )
        .unwrap();

        let closed = close_pr_at_path(
            &path,
            PrCloseInput {
                workspace_root: repo.path().to_str().unwrap(),
                session_dir: ".sessions",
                title: "traced PR",
                url: None,
                status: "manual",
                quality: None,
                ship_record_path: None,
                quality_threshold: DEFAULT_QUALITY_PR_THRESHOLD,
            },
            DateTime::parse_from_rfc3339("2026-06-14T12:00:00Z")
                .unwrap()
                .with_timezone(&Utc),
        )
        .unwrap()
        .unwrap();

        assert_eq!(closed.pr.session_traces.len(), 1);
        assert!(closed.pr.session_traces[0].trace_found);
        assert_eq!(closed.pr.session_traces[0].turn_count, 1);
        assert_eq!(closed.pr.session_traces[0].total_steps, 2);
    }
}
