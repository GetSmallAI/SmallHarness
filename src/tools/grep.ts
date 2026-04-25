import { execFile } from 'child_process';
import { promisify } from 'util';
import { z } from 'zod';
import { tool } from './define.js';

const execFileP = promisify(execFile);

export const grepTool = tool({
  name: 'grep',
  description: 'Search file contents by regex. Uses ripgrep when available. Returns up to 100 matches.',
  inputSchema: z.object({
    pattern: z.string().describe('Regex pattern to search for'),
    path: z.string().optional().describe('Directory or file to search (default: cwd)'),
    glob: z.string().optional().describe('File filter, e.g. "*.ts"'),
    ignoreCase: z.boolean().optional(),
  }),
  execute: async ({ pattern, path, glob, ignoreCase }) => {
    try {
      const args = ['--line-number', '--no-heading', '--max-count=100', '--color=never'];
      if (ignoreCase) args.push('--ignore-case');
      if (glob) args.push('--glob', glob);
      args.push(pattern, path ?? '.');
      const { stdout } = await execFileP('rg', args, { maxBuffer: 1024 * 1024 });
      const matches = stdout.split('\n').filter(Boolean).slice(0, 100).map((line) => {
        const m = line.match(/^([^:]+):(\d+):(.*)$/);
        return m ? { file: m[1], line: Number(m[2]), content: m[3] } : { content: line };
      });
      return { matches, count: matches.length };
    } catch (err) {
      const e = err as { code?: number; stdout?: string; message: string };
      if (e.code === 1) return { matches: [], count: 0 };
      if (e.code === 'ENOENT' as unknown as number) {
        return { error: 'ripgrep (rg) not found. Install with `brew install ripgrep`.' };
      }
      return { error: e.message };
    }
  },
});
