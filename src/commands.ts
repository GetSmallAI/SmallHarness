import OpenAI from 'openai';
import { plainReadLine } from './input-styles.js';
import { BACKENDS, buildClient, isBackendName, isProfileName } from './backends.js';
import type { BackendName, ProfileName } from './backends.js';
import { ALL_TOOL_NAMES, isToolName, type AgentConfig, type ToolName } from './config.js';
import type { ChatMessage } from './agent.js';

const RESET = '\x1b[0m';
const DIM = '\x1b[2m';
const BOLD = '\x1b[1m';
const CYAN = '\x1b[36m';
const GREEN = '\x1b[32m';
const YELLOW = '\x1b[33m';
const RED = '\x1b[31m';

export interface CommandContext {
  config: AgentConfig;
  client: OpenAI;
  model: string;
  messages: ChatMessage[];
  sessionPath: string;
  totalTokens: { input: number; output: number };
  rebuildClient: () => void;
  resetSession: () => void;
  resolveModel: () => void;
}

interface SlashCommand {
  name: string;
  description: string;
  execute: (args: string, ctx: CommandContext) => Promise<void>;
}

const commands: SlashCommand[] = [];
const register = (cmd: SlashCommand) => commands.push(cmd);

export function listCommandNames(): string[] {
  return commands.map((c) => c.name);
}

export async function dispatch(input: string, ctx: CommandContext): Promise<void> {
  const [name, ...rest] = input.split(' ');
  const cmd = commands.find((c) => c.name === name);
  if (!cmd) {
    console.log(`  ${DIM}Unknown command: ${name}. Type /help.${RESET}`);
    return;
  }
  await cmd.execute(rest.join(' ').trim(), ctx);
}

register({
  name: '/help',
  description: 'List available commands',
  execute: async () => {
    for (const c of commands) {
      console.log(`  ${CYAN}${c.name.padEnd(12)}${RESET} ${DIM}${c.description}${RESET}`);
    }
    console.log(`  ${CYAN}${'exit'.padEnd(12)}${RESET} ${DIM}Quit${RESET}`);
  },
});

register({
  name: '/new',
  description: 'Start a fresh conversation',
  execute: async (_args, ctx) => {
    ctx.messages.length = 0;
    ctx.resetSession();
    console.log(`  ${GREEN}✓${RESET} ${DIM}New session started.${RESET}`);
  },
});

register({
  name: '/clear',
  description: 'Clear the screen',
  execute: async () => { process.stdout.write('\x1b[2J\x1b[H'); },
});

register({
  name: '/session',
  description: 'Show session info and token usage',
  execute: async (_args, ctx) => {
    const fmt = (n: number) => (n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n));
    console.log(`  ${DIM}backend${RESET}   ${CYAN}${ctx.config.backend}${RESET}`);
    console.log(`  ${DIM}profile${RESET}   ${CYAN}${ctx.config.profile}${RESET}`);
    console.log(`  ${DIM}model${RESET}     ${CYAN}${ctx.model}${RESET}`);
    console.log(`  ${DIM}approval${RESET}  ${CYAN}${ctx.config.approvalPolicy}${RESET}`);
    console.log(`  ${DIM}session${RESET}   ${ctx.sessionPath}`);
    console.log(`  ${DIM}messages${RESET}  ${ctx.messages.length}`);
    console.log(`  ${DIM}tokens${RESET}    ${fmt(ctx.totalTokens.input)} in · ${fmt(ctx.totalTokens.output)} out`);
  },
});

register({
  name: '/backend',
  description: 'Switch backend (ollama, lm-studio, mlx, openrouter)',
  execute: async (args, ctx) => {
    const choices: BackendName[] = ['ollama', 'lm-studio', 'mlx', 'openrouter'];
    let chosen: BackendName | undefined;
    if (args && isBackendName(args)) {
      chosen = args;
    } else {
      console.log(`  ${DIM}Current:${RESET} ${CYAN}${ctx.config.backend}${RESET}`);
      choices.forEach((b, i) => console.log(`  ${DIM}${i + 1})${RESET} ${b}`));
      const pick = (await plainReadLine(`  ${DIM}Select (1-${choices.length}):${RESET} `)).trim();
      const idx = parseInt(pick, 10) - 1;
      if (idx >= 0 && idx < choices.length) chosen = choices[idx];
    }
    if (!chosen) { console.log(`  ${DIM}Cancelled.${RESET}`); return; }
    if (chosen === 'openrouter' && !BACKENDS.openrouter.apiKey) {
      console.log(`  ${RED}✗${RESET} ${DIM}OPENROUTER_API_KEY not set in environment.${RESET}`);
      return;
    }
    ctx.config.backend = chosen;
    ctx.config.modelOverride = undefined;
    ctx.rebuildClient();
    ctx.resolveModel();
    console.log(`  ${GREEN}✓${RESET} ${DIM}backend →${RESET} ${CYAN}${chosen}${RESET} ${DIM}· model →${RESET} ${CYAN}${ctx.model}${RESET}`);
  },
});

register({
  name: '/profile',
  description: 'Switch hardware profile (mac-mini-16gb, mac-studio-32gb)',
  execute: async (args, ctx) => {
    const choices: ProfileName[] = ['mac-mini-16gb', 'mac-studio-32gb'];
    let chosen: ProfileName | undefined;
    if (args && isProfileName(args)) {
      chosen = args;
    } else {
      console.log(`  ${DIM}Current:${RESET} ${CYAN}${ctx.config.profile}${RESET}`);
      choices.forEach((p, i) => console.log(`  ${DIM}${i + 1})${RESET} ${p}`));
      const pick = (await plainReadLine(`  ${DIM}Select (1-${choices.length}):${RESET} `)).trim();
      const idx = parseInt(pick, 10) - 1;
      if (idx >= 0 && idx < choices.length) chosen = choices[idx];
    }
    if (!chosen) { console.log(`  ${DIM}Cancelled.${RESET}`); return; }
    ctx.config.profile = chosen;
    ctx.config.modelOverride = undefined;
    ctx.resolveModel();
    console.log(`  ${GREEN}✓${RESET} ${DIM}profile →${RESET} ${CYAN}${chosen}${RESET} ${DIM}· model →${RESET} ${CYAN}${ctx.model}${RESET}`);
  },
});

register({
  name: '/model',
  description: 'List models from the current backend and pick one',
  execute: async (args, ctx) => {
    if (args) {
      ctx.config.modelOverride = args;
      ctx.resolveModel();
      console.log(`  ${GREEN}✓${RESET} ${DIM}model →${RESET} ${CYAN}${ctx.model}${RESET}`);
      return;
    }
    process.stdout.write(`  ${DIM}Fetching models from ${ctx.config.backend}…${RESET}`);
    let ids: string[] = [];
    try {
      const res = await ctx.client.models.list();
      ids = res.data.map((m) => m.id);
    } catch (err) {
      process.stdout.write('\r\x1b[K');
      console.log(`  ${RED}✗${RESET} ${DIM}Failed: ${(err as Error).message}${RESET}`);
      return;
    }
    process.stdout.write('\r\x1b[K');
    if (!ids.length) { console.log(`  ${DIM}No models available.${RESET}`); return; }
    const filter = (await plainReadLine(`  ${DIM}Filter (blank for all):${RESET} `)).trim().toLowerCase();
    const matches = filter ? ids.filter((m) => m.toLowerCase().includes(filter)) : ids;
    const shown = matches.slice(0, 20);
    if (!shown.length) { console.log(`  ${DIM}No matches.${RESET}`); return; }
    shown.forEach((m, i) => console.log(`  ${DIM}${String(i + 1).padStart(2)})${RESET} ${m}`));
    if (matches.length > shown.length) console.log(`  ${DIM}…and ${matches.length - shown.length} more${RESET}`);
    const pick = (await plainReadLine(`  ${DIM}Select (1-${shown.length}):${RESET} `)).trim();
    const idx = parseInt(pick, 10) - 1;
    if (idx >= 0 && idx < shown.length) {
      ctx.config.modelOverride = shown[idx];
      ctx.resolveModel();
      console.log(`  ${GREEN}✓${RESET} ${DIM}model →${RESET} ${CYAN}${ctx.model}${RESET}`);
    } else {
      console.log(`  ${DIM}Cancelled.${RESET}`);
    }
  },
});

register({
  name: '/tools',
  description: 'Show or set enabled tools (comma-separated names)',
  execute: async (args, ctx) => {
    if (!args) {
      console.log(`  ${DIM}available${RESET}  ${ALL_TOOL_NAMES.join(', ')}`);
      console.log(`  ${DIM}enabled${RESET}    ${CYAN}${ctx.config.tools.join(', ')}${RESET}`);
      console.log(`  ${DIM}usage${RESET}      /tools file_read,grep,list_dir`);
      return;
    }
    const requested = args.split(',').map((s) => s.trim()).filter(Boolean);
    const invalid = requested.filter((n) => !isToolName(n));
    if (invalid.length) {
      console.log(`  ${RED}✗${RESET} ${DIM}unknown tools: ${invalid.join(', ')}${RESET}`);
      return;
    }
    ctx.config.tools = requested as ToolName[];
    console.log(`  ${GREEN}✓${RESET} ${DIM}tools →${RESET} ${CYAN}${ctx.config.tools.join(', ')}${RESET}`);
  },
});

register({
  name: '/compare',
  description: 'Run the last user prompt against the OpenRouter cloud (requires OPENROUTER_API_KEY)',
  execute: async (args, ctx) => {
    if (!BACKENDS.openrouter.apiKey) {
      console.log(`  ${RED}✗${RESET} ${DIM}OPENROUTER_API_KEY not set.${RESET}`);
      return;
    }
    const lastUser = [...ctx.messages].reverse().find((m) => m.role === 'user');
    if (!lastUser) { console.log(`  ${DIM}No user message yet.${RESET}`); return; }
    const cloudModel = args.trim() || BACKENDS.openrouter.defaultModelByProfile[ctx.config.profile];
    const cloudClient = buildClient(BACKENDS.openrouter);
    const userText = typeof lastUser.content === 'string'
      ? lastUser.content
      : Array.isArray(lastUser.content)
        ? lastUser.content.map((c) => ('text' in c ? c.text : '')).join('')
        : '';
    console.log(`  ${YELLOW}⇆${RESET} ${BOLD}cloud${RESET} ${DIM}${cloudModel}${RESET}`);
    process.stdout.write('\n');
    try {
      const stream = await cloudClient.chat.completions.create({
        model: cloudModel,
        messages: [{ role: 'system', content: ctx.config.systemPrompt.replace('{cwd}', process.cwd()).replace('{tools}', ctx.config.tools.join(', ')) }, { role: 'user', content: userText }],
        stream: true,
      });
      for await (const chunk of stream) {
        const d = chunk.choices?.[0]?.delta?.content;
        if (d) process.stdout.write(d);
      }
      process.stdout.write('\n');
    } catch (err) {
      console.log(`  ${RED}✗${RESET} ${DIM}${(err as Error).message}${RESET}`);
    }
  },
});

