import { readFile, writeFile } from 'fs/promises';
import { z } from 'zod';
import { tool } from './define.js';

function unifiedDiff(oldText: string, newText: string, path: string): string {
  const oldLines = oldText.split('\n');
  const newLines = newText.split('\n');
  const lines: string[] = [`--- ${path}`, `+++ ${path}`];
  let i = 0, j = 0;
  while (i < oldLines.length || j < newLines.length) {
    if (oldLines[i] === newLines[j]) { i++; j++; continue; }
    const hunk: string[] = [`@@ -${i + 1} +${j + 1} @@`];
    while (i < oldLines.length && oldLines[i] !== newLines[j]) { hunk.push(`-${oldLines[i++]}`); }
    while (j < newLines.length && oldLines[i] !== newLines[j]) { hunk.push(`+${newLines[j++]}`); }
    lines.push(...hunk);
  }
  return lines.join('\n');
}

export function createFileEditTool(requireApproval: boolean) {
  return tool({
    name: 'file_edit',
    description: 'Apply search-and-replace edits to a file. Each old_text must appear exactly once. Returns a unified diff.',
    inputSchema: z.object({
      path: z.string().describe('Absolute or relative path to the file'),
      edits: z.array(z.object({
        old_text: z.string().describe('Exact text to find (must appear once)'),
        new_text: z.string().describe('Text to replace it with'),
      })).min(1),
    }),
    requireApproval,
    execute: async ({ path, edits }) => {
      try {
        const original = await readFile(path, 'utf-8');
        let working = original;
        for (const [idx, e] of edits.entries()) {
          if (!e.old_text) return { error: `Edit ${idx + 1}: old_text is empty` };
          const occurrences = working.split(e.old_text).length - 1;
          if (occurrences === 0) return { error: `Edit ${idx + 1}: old_text not found` };
          if (occurrences > 1) return { error: `Edit ${idx + 1}: old_text appears ${occurrences} times — make it unique` };
          working = working.replace(e.old_text, e.new_text);
        }
        await writeFile(path, working, 'utf-8');
        return { edited: true, path, diff: unifiedDiff(original, working, path) };
      } catch (err) {
        const e = err as NodeJS.ErrnoException;
        if (e.code === 'ENOENT') return { error: `File not found: ${path}` };
        return { error: e.message };
      }
    },
  });
}
