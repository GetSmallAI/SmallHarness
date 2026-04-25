import { writeFile, mkdir } from 'fs/promises';
import { dirname } from 'path';
import { z } from 'zod';
import { tool } from './define.js';

export function createFileWriteTool(requireApproval: boolean) {
  return tool({
    name: 'file_write',
    description: 'Write content to a file. Creates parent directories if needed. Overwrites if the file exists.',
    inputSchema: z.object({
      path: z.string().describe('Absolute or relative path to the file'),
      content: z.string().describe('Full content to write'),
    }),
    requireApproval,
    execute: async ({ path, content }) => {
      try {
        await mkdir(dirname(path), { recursive: true });
        await writeFile(path, content, 'utf-8');
        return { written: true, path, bytes: Buffer.byteLength(content, 'utf-8') };
      } catch (err) {
        return { error: (err as Error).message };
      }
    },
  });
}
