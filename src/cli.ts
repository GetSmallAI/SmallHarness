import OpenAI from 'openai';
import { loadConfig } from './config.js';
import { BACKENDS, buildClient, defaultModel } from './backends.js';
import { runAgent, type AgentEvent, type ChatMessage } from './agent.js';
import { buildTools } from './tools/index.js';
import { TuiRenderer } from './renderer.js';
import { Loader } from './loader.js';
import { printBanner } from './banner.js';
import { borderedReadLine, plainReadLine } from './input-styles.js';
import { initSessionDir, newSessionPath, saveMessage } from './session.js';
import { newApprovalCache, askApproval } from './approval.js';
import { dispatch, type CommandContext } from './commands.js';

const RESET = '\x1b[0m';
const DIM = '\x1b[2m';
const GREEN = '\x1b[32m';
const YELLOW = '\x1b[33m';
const RED = '\x1b[31m';
const GRAY = '\x1b[90m';

function formatTokens(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

async function probeBackend(client: OpenAI, backendName: string): Promise<{ ok: boolean; hint?: string }> {
  try {
    await client.models.list();
    return { ok: true };
  } catch (err) {
    const msg = (err as Error).message;
    const hints: Record<string, string> = {
      ollama: 'Is `ollama serve` running? Default port is 11434.',
      'lm-studio': 'Open LM Studio → "Local Server" tab → Start Server. Default port is 1234.',
      mlx: 'Start an MLX OpenAI-compatible server (e.g. `mlx_lm.server`). Default port is 8080.',
      openrouter: 'Check OPENROUTER_API_KEY.',
    };
    return { ok: false, hint: `${msg}. ${hints[backendName] ?? ''}` };
  }
}

async function main() {
  if (!process.stdin.isTTY) {
    console.error('small-harness requires an interactive TTY (run it directly in a terminal, not piped).');
    process.exit(1);
  }
  const config = loadConfig();
  const ctxState: { client: OpenAI; model: string } = {
    client: buildClient(BACKENDS[config.backend]),
    model: defaultModel(BACKENDS[config.backend], config.profile, config.modelOverride),
  };

  printBanner({
    backend: config.backend,
    profile: config.profile,
    model: ctxState.model,
    approval: config.approvalPolicy,
  });

  const probe = await probeBackend(ctxState.client, config.backend);
  if (!probe.ok) {
    console.log(`  ${YELLOW}!${RESET} ${DIM}Backend not reachable: ${probe.hint}${RESET}`);
    console.log(`  ${DIM}You can still type /backend to switch, or fix and retry.${RESET}`);
  }

  initSessionDir(config.sessionDir);
  let sessionPath = newSessionPath(config.sessionDir);
  const messages: ChatMessage[] = [];
  const totalTokens = { input: 0, output: 0 };
  const approvalCache = newApprovalCache();

  const cmdCtx: CommandContext = {
    config,
    get client() { return ctxState.client; },
    set client(c) { ctxState.client = c; },
    get model() { return ctxState.model; },
    set model(m) { ctxState.model = m; },
    messages,
    sessionPath,
    totalTokens,
    rebuildClient: () => { ctxState.client = buildClient(BACKENDS[config.backend]); },
    resetSession: () => { sessionPath = newSessionPath(config.sessionDir); cmdCtx.sessionPath = sessionPath; },
    resolveModel: () => { ctxState.model = defaultModel(BACKENDS[config.backend], config.profile, config.modelOverride); },
  } as CommandContext;

  const renderer = new TuiRenderer(config.display);

  while (true) {
    const input = config.display.inputStyle === 'bordered'
      ? await borderedReadLine()
      : await plainReadLine(`${GREEN}>${RESET} `);
    const trimmed = input.trim();
    if (!trimmed) continue;

    if (config.display.inputStyle === 'bordered') {
      const cwd = process.cwd().replace(process.env.HOME ?? '', '~');
      process.stdout.write(`  ${DIM}${cwd}${RESET}\n`);
    }

    if (trimmed === 'exit' || trimmed === 'quit' || trimmed === '.exit') {
      console.log(`  ${DIM}bye.${RESET}`);
      process.exit(0);
    }

    if (trimmed.startsWith('/')) {
      try { await dispatch(trimmed, cmdCtx); }
      catch (err) { console.log(`  ${RED}✗${RESET} ${DIM}${(err as Error).message}${RESET}`); }
      continue;
    }

    if (messages.length === 0) {
      const sys: ChatMessage = { role: 'system', content: config.systemPrompt.replace('{cwd}', process.cwd()).replace('{tools}', config.tools.join(', ')) };
      messages.push(sys);
      saveMessage(sessionPath, sys);
    }
    const userMsg: ChatMessage = { role: 'user', content: trimmed };
    messages.push(userMsg);
    saveMessage(sessionPath, userMsg);

    const tools = buildTools(config);
    const loader = new Loader(config.display.loaderText, config.display.loaderStyle);
    let loaderActive = true;
    loader.start();

    const handleEvent = (event: AgentEvent) => {
      if (loaderActive) { loader.stop(); loaderActive = false; }
      renderer.handle(event);
    };

    try {
      const before = messages.length;
      const result = await runAgent(
        ctxState.client,
        ctxState.model,
        messages,
        tools,
        {
          onEvent: handleEvent,
          maxSteps: config.maxSteps,
          approve: async (name, args) => {
            if (loaderActive) { loader.stop(); loaderActive = false; }
            renderer.endTurn();
            const ok = await askApproval(name, args, approvalCache);
            return ok;
          },
        },
      );
      if (loaderActive) { loader.stop(); loaderActive = false; }
      renderer.endTurn();

      // Persist new messages added during the turn
      messages.length = 0;
      messages.push(...result.messages);
      for (let i = before; i < result.messages.length; i++) {
        saveMessage(sessionPath, result.messages[i]);
      }

      totalTokens.input += result.usage.inputTokens;
      totalTokens.output += result.usage.outputTokens;
      console.log(`${GRAY}  ${formatTokens(result.usage.inputTokens)} in · ${formatTokens(result.usage.outputTokens)} out${RESET}`);
    } catch (err) {
      if (loaderActive) loader.stop();
      renderer.endTurn();
      console.log(`  ${RED}✗${RESET} ${DIM}${(err as Error).message}${RESET}`);
    }
  }
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
