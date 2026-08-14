/**
 * Pure formatting helpers for rendering memories in the tree view / detail
 * document. No `vscode` import here on purpose — this module is unit-testable
 * without spinning up the VS Code Extension Host.
 */

export interface Memory {
  id: string;
  memory_type: string;
  content: string;
  instruction: string | null;
  priority: string;
  namespace: string;
  tags: string[];
  layer: string;
  skill_meta: { trigger: string | null; steps: string[]; verification: string | null; version: number } | null;
  access_count: number;
  human_reviewed: boolean;
  created_at: string;
  updated_at: string;
}

export function priorityIcon(priority: string): string {
  return priority === 'MUST' ? '🔴' : priority === 'REFERENCE' ? '🔵' : '⚪';
}

export function treeItemLabel(mem: Memory, maxLen = 60): string {
  return `${priorityIcon(mem.priority)} ${mem.content.slice(0, maxLen)}`;
}

export function treeItemDescription(mem: Memory): string {
  return `[${mem.layer}] ${mem.memory_type}`;
}

export function treeItemTooltipLines(mem: Memory): string[] {
  const lines = [
    `ID: ${mem.id}`,
    `Priority: ${mem.priority} | Layer: ${mem.layer}`,
    `Type: ${mem.memory_type}`,
    `Tags: ${mem.tags.join(', ') || 'none'}`,
    `Namespace: ${mem.namespace}`,
    `Access: ${mem.access_count} | Reviewed: ${mem.human_reviewed}`,
  ];
  if (mem.instruction) lines.push(`Instruction: ${mem.instruction}`);
  if (mem.skill_meta) {
    lines.push(`Skill trigger: ${mem.skill_meta.trigger || 'none'}`);
    lines.push(`Skill steps: ${mem.skill_meta.steps.join(' → ') || 'none'}`);
    if (mem.skill_meta.verification) lines.push(`Verification: ${mem.skill_meta.verification}`);
  }
  return lines;
}

export function formatMemoryDetail(mem: Memory): string {
  let text = `# Memory: ${mem.id}\n\n`;
  text += `priority: ${mem.priority}\n`;
  text += `layer: ${mem.layer}\n`;
  text += `type: ${mem.memory_type}\n`;
  text += `namespace: ${mem.namespace}\n`;
  text += `tags: [${mem.tags.join(', ')}]\n`;
  text += `reviewed: ${mem.human_reviewed}\n`;
  text += `access_count: ${mem.access_count}\n`;
  text += `created_at: ${mem.created_at}\n\n`;
  text += `## Content\n${mem.content}\n`;
  if (mem.instruction) text += `\n## Instruction\n${mem.instruction}\n`;
  if (mem.skill_meta) {
    text += `\n## Skill Meta\n`;
    text += `trigger: ${mem.skill_meta.trigger || 'none'}\n`;
    text += `steps:\n${mem.skill_meta.steps.map((s, i) => `  ${i + 1}. ${s}`).join('\n')}\n`;
    if (mem.skill_meta.verification) text += `verification: ${mem.skill_meta.verification}\n`;
    text += `version: ${mem.skill_meta.version}\n`;
  }
  return text;
}
