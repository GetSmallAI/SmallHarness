const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[2m";
const CYAN: &str = "\x1b[36m";
const GRAY: &str = "\x1b[90m";

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

pub fn print_banner(info: BannerInfo<'_>) {
    println!();
    println!("{CYAN}{BOLD}{LOGO}{RESET}");
    println!();
    println!("   {BOLD}Small Harness{RESET}  {DIM}local LLM TUI{RESET}");
    println!(
        "   {DIM}backend{RESET}   {CYAN}{}{RESET}  {DIM}·{RESET}  {DIM}profile{RESET}  {CYAN}{}{RESET}",
        info.backend, info.profile
    );
    println!("   {DIM}model{RESET}     {CYAN}{}{RESET}", info.model);
    println!("   {DIM}approval{RESET}  {CYAN}{}{RESET}", info.approval);
    println!(
        "   {GRAY}/help for commands · /backend, /profile, /model to switch · exit to quit{RESET}"
    );
    println!();
}
