import { appendFileSync, existsSync, mkdirSync, readFileSync, readdirSync } from 'fs';
import { join } from 'path';
import type { ChatMessage } from './agent.js';

interface SessionEntry {
  timestamp: string;
  message: ChatMessage;
}

export function initSessionDir(dir: string): void {
  if (!existsSync(dir)) mkdirSync(dir, { recursive: true });
}

export function newSessionPath(dir: string): string {
  const id = new Date().toISOString().replace(/[:.]/g, '-');
  return join(dir, `${id}.jsonl`);
}

export function saveMessage(sessionPath: string, message: ChatMessage): void {
  const entry: SessionEntry = { timestamp: new Date().toISOString(), message };
  appendFileSync(sessionPath, JSON.stringify(entry) + '\n');
}

export function loadSession(sessionPath: string): ChatMessage[] {
  if (!existsSync(sessionPath)) return [];
  return readFileSync(sessionPath, 'utf-8')
    .split('\n')
    .filter(Boolean)
    .map((line) => {
      try { return (JSON.parse(line) as SessionEntry).message; }
      catch { return null; }
    })
    .filter((m): m is ChatMessage => m !== null);
}

export function listSessions(dir: string): string[] {
  if (!existsSync(dir)) return [];
  return readdirSync(dir).filter((f) => f.endsWith('.jsonl')).sort();
}
