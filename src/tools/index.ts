import type { AgentConfig } from '../config.js';
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
  return [
    fileReadTool,
    createFileWriteTool(writeApproves),
    createFileEditTool(writeApproves),
    globTool,
    grepTool,
    listDirTool,
    createShellTool(config.approvalPolicy),
  ];
}
