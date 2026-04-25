import { readFileSync, existsSync } from 'fs';
import { resolve } from 'path';
import { isBackendName, isProfileName, type BackendName, type ProfileName } from './backends.js';

export type ApprovalPolicy = 'always' | 'never' | 'dangerous-only';

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
  display: DisplayConfig;
  slashCommands: boolean;
}

const SYSTEM_PROMPT = [
  'You are a coding assistant running on the user\'s local machine via a small LLM.',
  'You have tools to read, write, and edit files; search by glob/grep; list directories; and run shell commands.',
  '',
  'Current working directory: {cwd}',
  '',
  'Guidelines:',
  '- Use tools to gather information instead of asking the user.',
  '- Make minimal targeted edits consistent with existing style.',
  '- Be concise. The user can read the diff.',
  '- Show file paths clearly when working with files.',
  '- Prefer grep and glob over shell for file search.',
  '- Mutating tools (file_write, file_edit, shell) may require user approval — that is expected; wait for the result.',
].join('\n');

const DEFAULTS: AgentConfig = {
  backend: 'ollama',
  profile: 'mac-mini-16gb',
  systemPrompt: SYSTEM_PROMPT,
  maxSteps: 20,
  sessionDir: '.sessions',
  approvalPolicy: 'always',
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

  if (overrides.display) config.display = { ...config.display, ...overrides.display };
  config = { ...config, ...overrides, display: config.display };

  return config;
}
