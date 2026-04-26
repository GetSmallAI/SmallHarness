import OpenAI from 'openai';
import type { ChatCompletionTool } from 'openai/resources/chat/completions.js';

export interface WarmupArgs {
  client: OpenAI;
  model: string;
  systemPrompt: string;
  tools: ChatCompletionTool[];
}

// Send a minimal request with the same system prompt + tools we'll use
// for real turns. llama.cpp/Ollama cache the prompt-eval result for
// matching prefixes, so the first real user prompt only needs to evaluate
// the new user tokens — turns ~12s of cold prompt-eval into ~2s.
export async function warmup(args: WarmupArgs): Promise<{ ms: number }> {
  const start = Date.now();
  await args.client.chat.completions.create({
    model: args.model,
    messages: [
      { role: 'system', content: args.systemPrompt },
      { role: 'user', content: 'ok' },
    ],
    tools: args.tools.length ? args.tools : undefined,
    max_tokens: 1,
    stream: false,
  });
  return { ms: Date.now() - start };
}
