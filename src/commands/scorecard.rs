use super::*;
use std::path::Path;

pub(super) fn cmd_scorecard(args: &str, state: &AppState) -> Result<()> {
    if !state.config.scorecard.enabled {
        println!("  {DIM}scorecard disabled in config (scorecard.enabled = false){RESET}");
        return Ok(());
    }

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
            let rest: Vec<&str> = parts.collect();
            let (label, url, run_tests) = parse_scorecard_close_args(&rest)?;
            let snapshot = if run_tests {
                crate::shipcheck::collect_shipcheck_with_tests(&state.config.workspace_root, true)?
            } else {
                crate::shipcheck::collect_shipcheck(&state.config.workspace_root)?
            };
            let has_url = url.is_some();
            let quality =
                crate::shipcheck::quality_input_from_shipcheck(&snapshot, false, false, has_url);
            close_scorecard_pr(state, &label, url.as_deref(), "manual", Some(quality), None)?;
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
    quality: Option<crate::scorecard::PrQualityInput>,
    ship_record_path: Option<&Path>,
) -> Result<()> {
    if !state.config.scorecard.enabled {
        return Ok(());
    }

    let title = if label.trim().is_empty() {
        "Untitled PR"
    } else {
        label.trim()
    };
    let ship_record_path = ship_record_path.map(|path| path.display().to_string());
    match crate::scorecard::close_pr_for_workspace(crate::scorecard::PrCloseInput {
        workspace_root: &state.config.workspace_root,
        title,
        url,
        status,
        quality,
        ship_record_path: ship_record_path.as_deref(),
        quality_threshold: state.config.scorecard.quality_threshold,
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
            if !summary.pr.session_ids.is_empty() {
                println!(
                    "  {DIM}scorecard sessions{RESET}  {}",
                    summary.pr.session_ids.join(", ")
                );
            }
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

fn parse_scorecard_close_args(args: &[&str]) -> Result<(String, Option<String>, bool)> {
    let mut label_parts = Vec::new();
    let mut url = None;
    let mut run_tests = false;
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "--url" => {
                index += 1;
                if index >= args.len() {
                    anyhow::bail!("missing value for --url");
                }
                url = Some(args[index].to_string());
            }
            "--tests" => run_tests = true,
            value => label_parts.push(value),
        }
        index += 1;
    }
    Ok((label_parts.join(" "), url, run_tests))
}

fn print_scorecard_usage() {
    println!(
        "  {DIM}Usage: /scorecard [daily|current|prs [limit]|close <label> [--url <url>] [--tests]|path|reset --yes]{RESET}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_close_args_extracts_flags() {
        let (label, url, tests) = parse_scorecard_close_args(&[
            "OAuth",
            "login",
            "PR",
            "--url",
            "https://github.com/org/repo/pull/1",
            "--tests",
        ])
        .unwrap();
        assert_eq!(label, "OAuth login PR");
        assert_eq!(url.as_deref(), Some("https://github.com/org/repo/pull/1"));
        assert!(tests);
    }
}
