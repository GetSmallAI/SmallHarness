import { glob } from 'glob';
import { z } from 'zod';
import { tool } from './define.js';

export const globTool = tool({
  name: 'glob',
  description: 'Find files by glob pattern. Respects .gitignore. Returns up to 1000 paths.',
  inputSchema: z.object({
    pattern: z.string().describe('Glob pattern, e.g. "src/**/*.ts"'),
    path: z.string().optional().describe('Directory to search in (default: cwd)'),
  }),
  execute: async ({ pattern, path }) => {
    try {
      const results = await glob(pattern, {
        cwd: path ?? process.cwd(),
        ignore: ['**/node_modules/**', '**/.git/**', '**/dist/**'],
        nodir: true,
      });
      const truncated = results.length > 1000;
      return { matches: results.slice(0, 1000), count: results.length, truncated };
    } catch (err) {
      return { error: (err as Error).message };
    }
  },
});
