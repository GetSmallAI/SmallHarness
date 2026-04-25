import { readFile } from 'fs/promises';
import { z } from 'zod';
import { tool } from './define.js';

const IMAGE_EXT = /\.(jpe?g|png|gif|webp)$/i;

export const fileReadTool = tool({
  name: 'file_read',
  description: 'Read the contents of a file at the given path. Returns text or, for image files, base64 with mime type.',
  inputSchema: z.object({
    path: z.string().describe('Absolute or relative path to the file'),
    offset: z.number().int().positive().optional().describe('Start reading from this line (1-indexed)'),
    limit: z.number().int().positive().optional().describe('Maximum number of lines to return'),
  }),
  execute: async ({ path, offset, limit }) => {
    try {
      if (IMAGE_EXT.test(path)) {
        const buf = await readFile(path);
        const ext = (path.match(IMAGE_EXT)?.[1] ?? 'png').toLowerCase();
        const mimeType = ext === 'jpg' || ext === 'jpeg' ? 'image/jpeg' : `image/${ext}`;
        return { type: 'image', mimeType, data: buf.toString('base64') };
      }
      const content = await readFile(path, 'utf-8');
      const lines = content.split('\n');
      const start = offset ? offset - 1 : 0;
      const end = limit ? start + limit : lines.length;
      const slice = lines.slice(start, end);
      return {
        content: slice.join('\n'),
        totalLines: lines.length,
        ...(end < lines.length && { truncated: true, nextOffset: end + 1 }),
      };
    } catch (err) {
      const e = err as NodeJS.ErrnoException;
      if (e.code === 'ENOENT') return { error: `File not found: ${path}` };
      if (e.code === 'EACCES') return { error: `Permission denied: ${path}` };
      return { error: e.message };
    }
  },
});
