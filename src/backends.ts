import OpenAI from 'openai';

export type BackendName = 'ollama' | 'lm-studio' | 'mlx' | 'openrouter';
export type ProfileName = 'mac-mini-16gb' | 'mac-studio-32gb';

export interface BackendDescriptor {
  name: BackendName;
  baseURL: string;
  apiKey: string;
  isLocal: boolean;
  defaultModelByProfile: Record<ProfileName, string>;
}

const env = (k: string): string | undefined => process.env[k];

export const BACKENDS: Record<BackendName, BackendDescriptor> = {
  ollama: {
    name: 'ollama',
    baseURL: env('OLLAMA_BASE_URL') ?? 'http://localhost:11434/v1',
    apiKey: 'ollama',
    isLocal: true,
    defaultModelByProfile: {
      'mac-mini-16gb': 'qwen2.5-coder:7b',
      'mac-studio-32gb': 'qwen2.5-coder:14b',
    },
  },
  'lm-studio': {
    name: 'lm-studio',
    baseURL: env('LM_STUDIO_BASE_URL') ?? 'http://localhost:1234/v1',
    apiKey: 'lm-studio',
    isLocal: true,
    defaultModelByProfile: {
      'mac-mini-16gb': 'qwen2.5-coder-7b-instruct',
      'mac-studio-32gb': 'qwen2.5-coder-14b-instruct',
    },
  },
  mlx: {
    name: 'mlx',
    baseURL: env('MLX_BASE_URL') ?? 'http://localhost:8080/v1',
    apiKey: 'mlx',
    isLocal: true,
    defaultModelByProfile: {
      'mac-mini-16gb': 'mlx-community/Qwen2.5-Coder-7B-Instruct-4bit',
      'mac-studio-32gb': 'mlx-community/Qwen2.5-Coder-14B-Instruct-4bit',
    },
  },
  openrouter: {
    name: 'openrouter',
    baseURL: 'https://openrouter.ai/api/v1',
    apiKey: env('OPENROUTER_API_KEY') ?? '',
    isLocal: false,
    defaultModelByProfile: {
      'mac-mini-16gb': 'qwen/qwen-2.5-coder-32b-instruct',
      'mac-studio-32gb': 'qwen/qwen-2.5-coder-32b-instruct',
    },
  },
};

export function isBackendName(s: string): s is BackendName {
  return s === 'ollama' || s === 'lm-studio' || s === 'mlx' || s === 'openrouter';
}

export function isProfileName(s: string): s is ProfileName {
  return s === 'mac-mini-16gb' || s === 'mac-studio-32gb';
}

export function buildClient(b: BackendDescriptor): OpenAI {
  if (b.name === 'openrouter' && !b.apiKey) {
    throw new Error('OPENROUTER_API_KEY is required when BACKEND=openrouter.');
  }
  return new OpenAI({ baseURL: b.baseURL, apiKey: b.apiKey });
}

export function defaultModel(b: BackendDescriptor, p: ProfileName, override?: string): string {
  return override ?? b.defaultModelByProfile[p];
}
