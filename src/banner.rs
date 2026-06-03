use crate::theme::{rule, ACCENT, ACCENT_DEEP, BOLD, MUTED, PAD, RESET};

const LOGO: &str = r"   ███████╗███╗   ███╗ █████╗ ██╗     ██╗
   ██╔════╝████╗ ████║██╔══██╗██║     ██║
   ███████╗██╔████╔██║███████║██║     ██║
   ╚════██║██║╚██╔╝██║██╔══██║██║     ██║
   ███████║██║ ╚═╝ ██║██║  ██║███████╗███████╗
   ╚══════╝╚═╝     ╚═╝╚═╝  ╚═╝╚══════╝╚══════╝
   ██╗  ██╗ █████╗ ██████╗ ███╗   ██╗███████╗███████╗███████╗
   ██║  ██║██╔══██╗██╔══██╗████╗  ██║██╔════╝██╔════╝██╔════╝
   ███████║███████║██████╔╝██╔██╗ ██║█████╗  ███████╗███████╗
   ██╔══██║██╔══██║██╔══██╗██║╚██╗██║██╔══╝  ╚════██║╚════██║
   ██║  ██║██║  ██║██║  ██║██║ ╚████║███████╗███████║███████║
   ╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═╝╚═╝  ╚═══╝╚══════╝╚══════╝╚══════╝";

pub struct BannerInfo<'a> {
    pub model: &'a str,
    pub backend: &'a str,
    pub profile: &'a str,
    pub approval: &'a str,
}

/// One `label  value` row with an aligned, readable label.
fn row(label: &str, value: &str) -> String {
    format!("{PAD}{MUTED}{label:<9}{RESET}{ACCENT}{value}{RESET}")
}

pub fn print_banner(info: BannerInfo<'_>) {
    println!();
    println!("{ACCENT_DEEP}{BOLD}{LOGO}{RESET}");
    println!();
    println!("{PAD}{BOLD}Small Harness{RESET}  {MUTED}— a small, terminal-first coding harness{RESET}");
    println!();
    println!("{}", row("backend", info.backend));
    println!("{}", row("model", info.model));
    println!("{}", row("profile", info.profile));
    println!("{}", row("approval", info.approval));
    println!("{}", rule());
    println!(
        "{PAD}{MUTED}/help{RESET} commands  {MUTED}·{RESET}  {MUTED}/backend /model{RESET} switch  {MUTED}·{RESET}  {MUTED}exit{RESET} quit"
    );
    println!();
}
