import OpenAI from 'openai';
import type {
  ChatCompletionMessageParam,
  ChatCompletionTool,
  ChatCompletionMessageFunctionToolCall,
} from 'openai/resources/chat/completions.js';
import { z } from 'zod';
import type { Tool } from './tools/index.js';

export type AgentEvent =
  | { type: 'text'; delta: string }
  | { type: 'tool_call'; name: string; callId: string; args: Record<string, unknown> }
  | { type: 'tool_result'; name: string; callId: string; output: string }
  | { type: 'reasoning'; delta: string };

export type ChatMessage = ChatCompletionMessageParam;

export interface RunOptions {
  onEvent?: (e: AgentEvent) => void;
  signal?: AbortSignal;
  approve?: (name: string, args: Record<string, unknown>) => Promise<boolean>;
  maxSteps?: number;
}

export interface RunResult {
  messages: ChatMessage[];
  usage: { inputTokens: number; outputTokens: number };
}

function toOpenAITools(tools: Tool[]): ChatCompletionTool[] {
  return tools.map((t) => ({
    type: 'function',
    function: {
      name: t.name,
      description: t.description,
      parameters: z.toJSONSchema(t.inputSchema, { unrepresentable: 'any' }) as Record<string, unknown>,
    },
  }));
}

// Some local models (e.g. qwen2.5-coder via Ollama) emit tool-shaped JSON as
// plain content instead of using the API's tool_calls field. We detect that
// pattern and synthesize a real tool call so the harness still works.
function tryParseInlineToolCall(
  text: string,
  toolNames: Set<string>,
): { name: string; args: Record<string, unknown> } | null {
  const trimmed = text.trim().replace(/^```(?:json)?\s*|\s*```$/g, '').trim();
  if (!trimmed.startsWith('{')) return null;
  try {
    const parsed = JSON.parse(trimmed);
    if (typeof parsed.name === 'string' && toolNames.has(parsed.name)) {
      const args = parsed.arguments ?? parsed.parameters ?? parsed.args ?? {};
      if (args && typeof args === 'object') {
        return { name: parsed.name, args: args as Record<string, unknown> };
      }
    }
  } catch { /* not valid JSON */ }
  return null;
}

function looksLikeStartOfToolCall(text: string): boolean {
  return /^\s*(?:```(?:json)?\s*)?\{\s*"?name"?\s*:/.test(text);
}

export async function runAgent(
  client: OpenAI,
  model: string,
  initialMessages: ChatMessage[],
  tools: Tool[],
  options: RunOptions = {},
): Promise<RunResult> {
  const messages: ChatMessage[] = [...initialMessages];
  const toolMap = new Map(tools.map((t) => [t.name, t]));
  const toolDefs = toOpenAITools(tools);
  const maxSteps = options.maxSteps ?? 20;

  let inputTokens = 0;
  let outputTokens = 0;
  const toolNames = new Set(tools.map((t) => t.name));

  for (let step = 0; step < maxSteps; step++) {
    if (options.signal?.aborted) break;

    const stream = await client.chat.completions.create({
      model,
      messages,
      tools: toolDefs.length ? toolDefs : undefined,
      stream: true,
      stream_options: { include_usage: true },
    });

    let assistantText = '';
    let bufferingInline = false; // suspect inline tool-call JSON; defer streaming
    const toolCalls = new Map<number, { id: string; name: string; args: string }>();

    for await (const chunk of stream) {
      if (options.signal?.aborted) break;
      const choice = chunk.choices?.[0];
      if (choice?.delta?.content) {
        const wasEmpty = assistantText === '';
        assistantText += choice.delta.content;
        if (wasEmpty && looksLikeStartOfToolCall(assistantText)) bufferingInline = true;
        if (!bufferingInline) {
          options.onEvent?.({ type: 'text', delta: choice.delta.content });
        }
      }
      if (choice?.delta?.tool_calls) {
        for (const tc of choice.delta.tool_calls) {
          const idx = tc.index ?? 0;
          const existing = toolCalls.get(idx) ?? { id: '', name: '', args: '' };
          if (tc.id) existing.id = tc.id;
          if (tc.function?.name) existing.name += tc.function.name;
          if (tc.function?.arguments) existing.args += tc.function.arguments;
          toolCalls.set(idx, existing);
        }
      }
      if (chunk.usage) {
        inputTokens += chunk.usage.prompt_tokens ?? 0;
        outputTokens += chunk.usage.completion_tokens ?? 0;
      }
    }

    const finalToolCalls: ChatCompletionMessageFunctionToolCall[] = [...toolCalls.values()]
      .filter((tc) => tc.name && tc.id)
      .map((tc) => ({
        id: tc.id,
        type: 'function' as const,
        function: { name: tc.name, arguments: tc.args || '{}' },
      }));

    if (finalToolCalls.length === 0 && bufferingInline) {
      const inline = tryParseInlineToolCall(assistantText, toolNames);
      if (inline) {
        finalToolCalls.push({
          id: `inline-${step}-${Date.now()}`,
          type: 'function' as const,
          function: { name: inline.name, arguments: JSON.stringify(inline.args) },
        });
        assistantText = ''; // the JSON wasn't real assistant text
      } else {
        // Buffered but not a real tool call — flush the text now
        options.onEvent?.({ type: 'text', delta: assistantText });
      }
    }

    messages.push({
      role: 'assistant',
      content: assistantText || null,
      ...(finalToolCalls.length && { tool_calls: finalToolCalls }),
    });

    if (finalToolCalls.length === 0) break;

    for (const tc of finalToolCalls) {
      const tool = toolMap.get(tc.function.name);
      let parsed: Record<string, unknown> = {};
      try { parsed = tc.function.arguments ? JSON.parse(tc.function.arguments) : {}; }
      catch { parsed = {}; }

      options.onEvent?.({ type: 'tool_call', name: tc.function.name, callId: tc.id, args: parsed });

      let outputStr: string;
      if (!tool) {
        outputStr = JSON.stringify({ error: `Unknown tool: ${tc.function.name}` });
      } else {
        const needsApproval = typeof tool.requireApproval === 'function'
          ? tool.requireApproval(parsed as never)
          : tool.requireApproval === true;

        if (needsApproval && options.approve) {
          const ok = await options.approve(tc.function.name, parsed);
          if (!ok) {
            outputStr = JSON.stringify({ error: 'User denied execution.' });
            options.onEvent?.({ type: 'tool_result', name: tc.function.name, callId: tc.id, output: outputStr });
            messages.push({ role: 'tool', tool_call_id: tc.id, content: outputStr });
            continue;
          }
        }

        try {
          const validated = tool.inputSchema.parse(parsed);
          const result = await tool.execute(validated);
          outputStr = typeof result === 'string' ? result : JSON.stringify(result);
        } catch (err) {
          outputStr = JSON.stringify({ error: (err as Error).message });
        }
      }

      const trimmed = outputStr.length > 8000 ? outputStr.slice(0, 8000) + '…[truncated]' : outputStr;
      options.onEvent?.({ type: 'tool_result', name: tc.function.name, callId: tc.id, output: trimmed });
      messages.push({ role: 'tool', tool_call_id: tc.id, content: trimmed });
    }
  }

  return { messages, usage: { inputTokens, outputTokens } };
}
