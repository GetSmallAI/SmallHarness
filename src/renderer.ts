import type { AgentEvent } from './agent.js';
import type { DisplayConfig } from './config.js';

const RESET = '\x1b[0m';
const DIM = '\x1b[2m';
const BOLD = '\x1b[1m';
const GREEN = '\x1b[32m';
const YELLOW = '\x1b[33m';
const RED = '\x1b[31m';
const GRAY = '\x1b[90m';
const MAGENTA = '\x1b[35m';

type ToolFormatter = (name: string, args: Record<string, unknown>) => string;

const FORMATTERS: Record<string, ToolFormatter> = {
  shell: (_n, a) => `command=${trunc(String(a.command ?? ''))}`,
  file_read: (_n, a) => `path=${trunc(String(a.path ?? ''))}`,
  file_write: (_n, a) => `path=${trunc(String(a.path ?? ''))}`,
  file_edit: (_n, a) => `path=${trunc(String(a.path ?? ''))}`,
  glob: (_n, a) => `pattern=${trunc(String(a.pattern ?? ''))}`,
  grep: (_n, a) => `pattern=${trunc(String(a.pattern ?? ''))}`,
  list_dir: (_n, a) => `path=${trunc(String(a.path ?? '.'))}`,
};

const LABELS: Record<string, { past: string; noun: string }> = {
  shell: { past: 'Ran', noun: 'shell command' },
  file_read: { past: 'Read', noun: 'file' },
  file_write: { past: 'Wrote', noun: 'file' },
  file_edit: { past: 'Edited', noun: 'file' },
  glob: { past: 'Explored', noun: 'pattern' },
  grep: { past: 'Searched', noun: 'pattern' },
  list_dir: { past: 'Listed', noun: 'directory' },
};

const TOOL_COLORS: Record<string, string> = {
  shell: RED,
  file_write: YELLOW,
  file_edit: YELLOW,
  grep: MAGENTA,
};

function trunc(s: string, max = 50): string {
  return s.length > max ? s.slice(0, max - 1) + '…' : s;
}

interface PendingCall {
  name: string;
  callId: string;
  args: Record<string, unknown>;
  output?: string;
}

export class TuiRenderer {
  private toolStart = new Map<string, number>();
  private streaming = false;
  private groupedPending: PendingCall[] = [];
  private groupedCategory = '';
  private minimalBatch = new Map<string, number>();

  constructor(private display: DisplayConfig) {}

  handle(event: AgentEvent): void {
    switch (event.type) {
      case 'text': return this.renderText(event.delta);
      case 'tool_call': return this.renderToolCall(event.name, event.callId, event.args);
      case 'tool_result': return this.renderToolResult(event.name, event.callId, event.output);
      case 'reasoning': return this.renderReasoning(event.delta);
    }
  }

  endTurn(): void {
    this.flushGrouped();
    this.flushMinimal();
    this.endStreaming();
  }

  endStreaming(): void {
    if (this.streaming) {
      process.stdout.write(RESET + '\n');
      this.streaming = false;
    }
  }

  private renderText(delta: string): void {
    this.flushMinimal();
    this.streaming = true;
    process.stdout.write(delta);
  }

  private renderReasoning(delta: string): void {
    if (!this.display.reasoning) return;
    this.endStreaming();
    process.stdout.write(`${DIM}${delta}${RESET}`);
  }

  private renderToolCall(name: string, callId: string, args: Record<string, unknown>): void {
    if (this.display.toolDisplay === 'hidden') return;
    this.endStreaming();
    this.toolStart.set(callId, Date.now());

    if (this.display.toolDisplay === 'emoji') {
      const color = TOOL_COLORS[name] ?? YELLOW;
      const fmt = FORMATTERS[name];
      const argStr = fmt ? fmt(name, args) : defaultFormat(args);
      console.log(`  ${color}⚡${RESET} ${DIM}${name}${argStr ? ' ' + argStr : ''}${RESET}`);
    } else if (this.display.toolDisplay === 'grouped') {
      const category = LABELS[name]?.past ?? name;
      if (category !== this.groupedCategory) {
        this.flushGrouped();
        this.groupedCategory = category;
      }
      this.groupedPending.push({ name, callId, args });
    } else if (this.display.toolDisplay === 'minimal') {
      this.minimalBatch.set(name, (this.minimalBatch.get(name) ?? 0) + 1);
    }
  }

  private renderToolResult(name: string, callId: string, output: string): void {
    if (this.display.toolDisplay === 'hidden') return;
    const ms = Date.now() - (this.toolStart.get(callId) ?? Date.now());
    const dur = `(${(ms / 1000).toFixed(1)}s)`;

    if (this.display.toolDisplay === 'emoji') {
      console.log(`  ${GREEN}✓${RESET} ${DIM}${name} ${dur}${RESET}`);
    } else if (this.display.toolDisplay === 'grouped') {
      const pending = this.groupedPending.find((p) => p.callId === callId);
      if (pending) pending.output = output;
    }
  }

  private flushGrouped(): void {
    if (this.groupedPending.length === 0) return;
    const first = this.groupedPending[0];
    const label = LABELS[first.name]?.past ?? first.name;
    const fmt = FORMATTERS[first.name];

    if (this.groupedPending.length === 1) {
      const argStr = fmt ? fmt(first.name, first.args) : defaultFormat(first.args);
      console.log(`${GREEN}●${RESET} ${BOLD}${label}${RESET} ${DIM}${argStr}${RESET}`);
      if (first.output) {
        const summary = summarizeOutput(first.output);
        if (summary) console.log(`  ${GRAY}└ ${summary}${RESET}`);
      }
    } else {
      console.log(`${GREEN}●${RESET} ${BOLD}${label}${RESET}`);
      for (let i = 0; i < this.groupedPending.length; i++) {
        const p = this.groupedPending[i];
        const isLast = i === this.groupedPending.length - 1;
        const branch = isLast ? '└' : '├';
        const f = FORMATTERS[p.name];
        const argStr = f ? f(p.name, p.args) : defaultFormat(p.args);
        const summary = p.output ? ` ${GRAY}${summarizeOutput(p.output)}${RESET}` : '';
        console.log(`  ${GRAY}${branch}${RESET} ${DIM}${argStr}${RESET}${summary}`);
      }
    }
    console.log();
    this.groupedPending = [];
    this.groupedCategory = '';
  }

  private flushMinimal(): void {
    if (this.minimalBatch.size === 0) return;
    const parts: string[] = [];
    for (const [name, count] of this.minimalBatch) {
      const lbl = LABELS[name];
      parts.push(lbl ? `${lbl.past.toLowerCase()} ${count} ${lbl.noun}${count !== 1 ? 's' : ''}` : `${count} ${name}`);
    }
    console.log(`  ${GRAY}${parts.join(', ')}${RESET}`);
    this.minimalBatch.clear();
  }
}

function defaultFormat(args: Record<string, unknown>): string {
  const key = Object.keys(args)[0];
  if (!key) return '';
  return `${key}=${trunc(String(args[key]))}`;
}

function summarizeOutput(output: string): string {
  try {
    const parsed = JSON.parse(output);
    if (parsed.error) return `${RED}error: ${trunc(parsed.error, 60)}${RESET}`;
    if (typeof parsed.totalLines === 'number') return `${parsed.totalLines} lines`;
    if (typeof parsed.count === 'number') return `${parsed.count} ${parsed.matches ? 'matches' : 'entries'}`;
    if (parsed.written) return `wrote ${parsed.bytes} bytes`;
    if (parsed.edited) return 'edited';
    if (typeof parsed.exitCode === 'number') return `exit ${parsed.exitCode}${parsed.timedOut ? ' (timeout)' : ''}`;
  } catch { /* not JSON */ }
  return trunc(output.split('\n')[0], 60);
}
