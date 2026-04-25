import { readFileSync, existsSync } from 'fs';
import { resolve } from 'path';
import { isBackendName, isProfileName, type BackendName, type ProfileName } from './backends.js';

export type ApprovalPolicy = 'always' | 'never' | 'dangerous-only';

export const ALL_TOOL_NAMES = [
  'file_read', 'file_write', 'file_edit', 'glob', 'grep', 'list_dir', 'shell',
] as const;
export type ToolName = typeof ALL_TOOL_NAMES[number];

export function isToolName(s: string): s is ToolName {
  return (ALL_TOOL_NAMES as readonly string[]).includes(s);
}

export interface DisplayConfig {
  toolDisplay: 'emoji' | 'grouped' | 'minimal' | 'hidden';
  inputStyle: 'block' | 'bordered' | 'plain';
  loaderText: string;
  loaderStyle: 'gradient' | 'spinner' | 'minimal';
  reasoning: boolean;
  showBanner: boolean;
}

export interface AgentConfig {
  backend: BackendName;
  profile: ProfileName;
  modelOverride?: string;
  systemPrompt: string;
  maxSteps: number;
  sessionDir: string;
  approvalPolicy: ApprovalPolicy;
  tools: ToolName[];
  display: DisplayConfig;
  slashCommands: boolean;
}

const SYSTEM_PROMPT = [
  'You are a coding assistant running on the user\'s local machine via a small open-weight LLM.',
  '',
  'Available tools: {tools}.',
  '',
  'When to use tools vs answer directly:',
  '- For greetings, casual chat, or questions you can answer from general knowledge, respond in plain text. Do NOT call a tool.',
  '- Only call a tool when the user\'s request actually needs filesystem access.',
  '- When you do call a tool, emit a real tool call — not a JSON description in your text response.',
  '',
  'When working with code:',
  '- Make minimal targeted edits consistent with existing style.',
  '- Be concise. The user can read the diff.',
  '',
  'Current working directory: {cwd}',
].join('\n');

const DEFAULTS: AgentConfig = {
  backend: 'ollama',
  profile: 'mac-mini-16gb',
  systemPrompt: SYSTEM_PROMPT,
  maxSteps: 20,
  sessionDir: '.sessions',
  approvalPolicy: 'always',
  tools: ['file_read', 'file_edit', 'grep', 'list_dir'],
  display: {
    toolDisplay: 'grouped',
    inputStyle: 'bordered',
    loaderText: 'Thinking',
    loaderStyle: 'spinner',
    reasoning: false,
    showBanner: true,
  },
  slashCommands: true,
};

function isApprovalPolicy(s: string): s is ApprovalPolicy {
  return s === 'always' || s === 'never' || s === 'dangerous-only';
}

export function loadConfig(overrides: Partial<AgentConfig> = {}): AgentConfig {
  let config: AgentConfig = { ...DEFAULTS, display: { ...DEFAULTS.display } };

  const configPath = resolve('agent.config.json');
  if (existsSync(configPath)) {
    const file = JSON.parse(readFileSync(configPath, 'utf-8'));
    if (file.display) config.display = { ...config.display, ...file.display };
    config = { ...config, ...file, display: config.display };
  }

  const envBackend = process.env.BACKEND;
  if (envBackend && isBackendName(envBackend)) config.backend = envBackend;

  const envProfile = process.env.PROFILE;
  if (envProfile && isProfileName(envProfile)) config.profile = envProfile;

  if (process.env.AGENT_MODEL) config.modelOverride = process.env.AGENT_MODEL;
  if (process.env.AGENT_MAX_STEPS) config.maxSteps = Number(process.env.AGENT_MAX_STEPS);

  const envApproval = process.env.APPROVAL_POLICY;
  if (envApproval && isApprovalPolicy(envApproval)) config.approvalPolicy = envApproval;

  if (process.env.AGENT_TOOLS) {
    const requested = process.env.AGENT_TOOLS.split(',').map((s) => s.trim()).filter(Boolean);
    const valid = requested.filter(isToolName);
    if (valid.length) config.tools = valid;
  }

  if (overrides.display) config.display = { ...config.display, ...overrides.display };
  config = { ...config, ...overrides, display: config.display };

  return config;
}
