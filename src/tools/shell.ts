import { execFile } from 'child_process';
import { z } from 'zod';
import { tool } from './define.js';
import type { ToolApprovalCheck } from './define.js';

const DANGEROUS = /\brm\b|\bsudo\b|\bchmod\b|\bchown\b|\bdd\b|\bmkfs\b|>\s*\/dev|--force\b|-rf?\b/;

export function createShellTool(policy: 'always' | 'never' | 'dangerous-only') {
  const requireApproval: ToolApprovalCheck<{ command: string; timeout?: number }> =
    policy === 'always' ? true
    : policy === 'never' ? false
    : ({ command }) => DANGEROUS.test(command);

  return tool({
    name: 'shell',
    description: 'Execute a shell command and return combined stdout/stderr. Output is truncated at 256KB.',
    inputSchema: z.object({
      command: z.string().describe('Shell command to execute'),
      timeout: z.number().int().positive().optional().describe('Timeout in seconds (default: 120)'),
    }),
    requireApproval,
    execute: async ({ command, timeout }) => {
      const shell = process.env.SHELL ?? '/bin/bash';
      const ms = (timeout ?? 120) * 1000;
      return new Promise((resolve) => {
        execFile(
          shell,
          ['-c', command],
          { timeout: ms, maxBuffer: 256 * 1024 },
          (err, stdout, stderr) => {
            const output = `${stdout ?? ''}${stderr ?? ''}`;
            if (err) {
              const e = err as NodeJS.ErrnoException & { signal?: string; killed?: boolean };
              if (e.killed && e.signal === 'SIGTERM') {
                resolve({ output: output || '(no output)', exitCode: -1, timedOut: true });
                return;
              }
              resolve({ output: output || e.message, exitCode: (e as { code?: number }).code ?? 1 });
              return;
            }
            const lines = output.split('\n');
            const truncated = lines.length > 2000;
            const finalOutput = truncated ? lines.slice(-2000).join('\n') : output;
            resolve({ output: finalOutput, exitCode: 0, ...(truncated && { truncated: true }) });
          },
        );
      });
    },
  });
}
