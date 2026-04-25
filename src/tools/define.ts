import type { z } from 'zod';

export type ToolApprovalCheck<T> = boolean | ((args: T) => boolean);

// T defaults to `any` so that `Tool[]` (heterogeneous schemas) is assignable
// without forcing every call site to widen. Runtime validation goes through
// `inputSchema.parse`, so we don't lose safety in practice.
export interface Tool<T = any> {
  name: string;
  description: string;
  inputSchema: z.ZodType<T>;
  requireApproval?: ToolApprovalCheck<T>;
  execute: (args: T) => Promise<unknown>;
}

export function tool<T>(def: Tool<T>): Tool<T> {
  return def;
}
