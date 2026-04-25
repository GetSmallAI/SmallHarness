import type { AgentConfig, ToolName } from '../config.js';
import type { Tool } from './define.js';
import { fileReadTool } from './file-read.js';
import { createFileWriteTool } from './file-write.js';
import { createFileEditTool } from './file-edit.js';
import { globTool } from './glob-tool.js';
import { grepTool } from './grep.js';
import { listDirTool } from './list-dir.js';
import { createShellTool } from './shell.js';

export type { Tool } from './define.js';

export function buildTools(config: AgentConfig): Tool[] {
  const writeApproves = config.approvalPolicy !== 'never';
  const all: Record<ToolName, Tool> = {
    file_read: fileReadTool,
    file_write: createFileWriteTool(writeApproves),
    file_edit: createFileEditTool(writeApproves),
    glob: globTool,
    grep: grepTool,
    list_dir: listDirTool,
    shell: createShellTool(config.approvalPolicy),
  };
  return config.tools.map((name) => all[name]);
}
