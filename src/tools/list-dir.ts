import { readdir } from 'fs/promises';
import { z } from 'zod';
import { tool } from './define.js';

export const listDirTool = tool({
  name: 'list_dir',
  description: 'List directory contents (alphabetical). Up to 500 entries.',
  inputSchema: z.object({
    path: z.string().optional().describe('Directory path (default: cwd)'),
  }),
  execute: async ({ path }) => {
    try {
      const entries = await readdir(path ?? '.', { withFileTypes: true });
      const sorted = entries
        .map((e) => (e.isDirectory() ? `${e.name}/` : e.name))
        .sort()
        .slice(0, 500);
      return { entries: sorted, count: sorted.length, truncated: entries.length > 500 };
    } catch (err) {
      const e = err as NodeJS.ErrnoException;
      if (e.code === 'ENOENT') return { error: `Directory not found: ${path}` };
      return { error: e.message };
    }
  },
});
