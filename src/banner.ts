const RESET = '\x1b[0m';
const BOLD = '\x1b[1m';
const DIM = '\x1b[2m';
const CYAN = '\x1b[36m';
const GRAY = '\x1b[90m';

const LOGO = `   ███████╗██╗  ██╗
   ██╔════╝██║  ██║
   ███████╗███████║
   ╚════██║██╔══██║
   ███████║██║  ██║
   ╚══════╝╚═╝  ╚═╝`;

export interface BannerInfo {
  model: string;
  backend: string;
  profile: string;
  approval: string;
}

export function printBanner(info: BannerInfo): void {
  console.log();
  console.log(CYAN + BOLD + LOGO + RESET);
  console.log();
  console.log(`   ${BOLD}small-harness${RESET}  ${DIM}local LLM TUI${RESET}`);
  console.log(`   ${DIM}backend${RESET}   ${CYAN}${info.backend}${RESET}  ${DIM}·${RESET}  ${DIM}profile${RESET}  ${CYAN}${info.profile}${RESET}`);
  console.log(`   ${DIM}model${RESET}     ${CYAN}${info.model}${RESET}`);
  console.log(`   ${DIM}approval${RESET}  ${CYAN}${info.approval}${RESET}`);
  console.log(`   ${GRAY}/help for commands · /backend, /profile, /model to switch · exit to quit${RESET}`);
  console.log();
}
