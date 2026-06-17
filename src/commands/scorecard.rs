use super::*;

pub(super) fn cmd_scorecard(args: &str, state: &AppState) -> Result<()> {
    let mut parts = args.split_whitespace();
    let action = parts.next().unwrap_or("daily");

    match action {
        "" | "daily" | "show" => {
            let report = crate::scorecard::load_report(&state.config.workspace_root)?;
            print!("{}", crate::scorecard::render_report(&report));
        }
        "current" => {
            let summary = crate::scorecard::current_summary(&state.config.workspace_root)?;
            print!("{}", crate::scorecard::render_current(&summary));
        }
        "prs" | "list" => {
            let limit = parts
                .next()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(12)
                .clamp(1, 50);
            let prs = crate::scorecard::recent_prs(limit)?;
            print!("{}", crate::scorecard::render_recent_prs(&prs));
        }
        "close" => {
            let label = parts.collect::<Vec<_>>().join(" ");
            close_scorecard_pr(state, &label, None, "manual", None)?;
        }
        "path" => match crate::scorecard::scorecard_path() {
            Some(path) => println!("  {DIM}scorecardStore{RESET} {}", path.display()),
            None => println!(
                "  {YELLOW}!{RESET} {DIM}scorecard store unavailable: HOME is unset{RESET}"
            ),
        },
        "reset" => {
            if parts.next() != Some("--yes") {
                println!("  {DIM}Usage: /scorecard reset --yes{RESET}");
                return Ok(());
            }
            match crate::scorecard::reset_store()? {
                Some(path) => println!(
                    "  {GREEN}✓{RESET} {DIM}scorecard reset:{RESET} {}",
                    path.display()
                ),
                None => println!(
                    "  {YELLOW}!{RESET} {DIM}scorecard store unavailable: HOME is unset{RESET}"
                ),
            }
        }
        _ => print_scorecard_usage(),
    }

    Ok(())
}

pub(super) fn close_scorecard_pr(
    state: &AppState,
    label: &str,
    url: Option<&str>,
    status: &str,
    quality: Option<crate::scorecard::PrQualityInput<'_>>,
) -> Result<()> {
    let title = if label.trim().is_empty() {
        "Untitled PR"
    } else {
        label.trim()
    };
    match crate::scorecard::close_pr_for_workspace(crate::scorecard::PrCloseInput {
        workspace_root: &state.config.workspace_root,
        title,
        url,
        status,
        quality,
    })? {
        Some(summary) => {
            let quality = summary
                .pr
                .quality
                .as_ref()
                .map(|quality| {
                    format!(
                        "{} {}/100 · {}",
                        quality.grade, quality.score, quality.status
                    )
                })
                .unwrap_or_else(|| "unrated quality".to_string());
            println!(
                "  {GREEN}✓{RESET} {DIM}scorecard PR closed:{RESET} {quality} · {} tokens · {} turn(s)",
                summary.pr.total_tokens, summary.pr.turn_count
            );
            println!(
                "  {GREEN}✓{RESET} {DIM}scorecard saved →{RESET} {}",
                summary.path.display()
            );
        }
        None => {
            println!(
                "  {YELLOW}!{RESET} {DIM}scorecard not closed: no tracked tokens for this repo/branch yet{RESET}"
            );
        }
    }
    Ok(())
}

fn print_scorecard_usage() {
    println!(
        "  {DIM}Usage: /scorecard [daily|current|prs [limit]|close <label>|path|reset --yes]{RESET}"
    );
}
