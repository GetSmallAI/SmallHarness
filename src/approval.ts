import { plainReadLine } from './input-styles.js';

const RESET = '\x1b[0m';
const BOLD = '\x1b[1m';
const DIM = '\x1b[2m';
const YELLOW = '\x1b[33m';
const RED = '\x1b[31m';
const GREEN = '\x1b[32m';

const SUMMARIZERS: Record<string, (a: Record<string, unknown>) => string> = {
  shell: (a) => String(a.command ?? ''),
  file_write: (a) => `${a.path} (${String(a.content ?? '').length} bytes)`,
  file_edit: (a) => {
    const edits = (a.edits as unknown[] | undefined) ?? [];
    return `${a.path} (${edits.length} edit${edits.length === 1 ? '' : 's'})`;
  },
};

export interface ApprovalCache {
  alwaysAllow: Set<string>;
}

export function newApprovalCache(): ApprovalCache {
  return { alwaysAllow: new Set() };
}

export async function askApproval(
  toolName: string,
  args: Record<string, unknown>,
  cache: ApprovalCache,
): Promise<boolean> {
  const cacheKey = `${toolName}:${args.command ?? args.path ?? ''}`;
  if (cache.alwaysAllow.has(toolName)) return true;
  if (cache.alwaysAllow.has(cacheKey)) return true;

  const summary = SUMMARIZERS[toolName]?.(args) ?? JSON.stringify(args);
  console.log();
  console.log(`  ${YELLOW}▲${RESET} ${BOLD}Approval required${RESET} ${DIM}for${RESET} ${BOLD}${toolName}${RESET}`);
  console.log(`    ${DIM}${summary}${RESET}`);
  console.log(`    ${DIM}[y]es · [n]o · [a]lways for ${toolName} · [s]ession-allow this exact call${RESET}`);
  const answer = (await plainReadLine(`  ${YELLOW}? ${RESET}`)).trim().toLowerCase();

  if (answer === 'a' || answer === 'always') {
    cache.alwaysAllow.add(toolName);
    console.log(`  ${GREEN}✓${RESET} ${DIM}allowing all ${toolName} calls this session${RESET}`);
    return true;
  }
  if (answer === 's') {
    cache.alwaysAllow.add(cacheKey);
    return true;
  }
  if (answer === 'y' || answer === 'yes') return true;
  console.log(`  ${RED}✗${RESET} ${DIM}denied${RESET}`);
  return false;
}
