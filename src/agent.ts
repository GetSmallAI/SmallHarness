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
    const toolCalls = new Map<number, { id: string; name: string; args: string }>();

    for await (const chunk of stream) {
      if (options.signal?.aborted) break;
      const choice = chunk.choices?.[0];
      if (choice?.delta?.content) {
        assistantText += choice.delta.content;
        options.onEvent?.({ type: 'text', delta: choice.delta.content });
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
