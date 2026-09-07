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

export interface DashboardStats {
  total: number;
  must_count: number;
  reference_count: number;
  reviewed_count: number;
  agents: string[];
  namespaces: string[];
  layers: { l0: number; l1: number; l2: number; l3: number };
  skills: number;
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

export interface ExtractedCandidate {
  content: string;
  instruction: string | null;
  type: string;
  priority: string;
  tags: string[];
  confidence: number;
}

export interface ExtractCoverage {
  input_lines: number;
  empty_lines: number;
  extracted_lines: number;
  no_signal_lines: number;
}

export function extractedCandidateLabel(c: ExtractedCandidate, maxLen = 70): string {
  return `${priorityIcon(c.priority)} [${c.type}] ${c.content.slice(0, maxLen)}`;
}

export function formatCoverageMessage(coverage: ExtractCoverage): string {
  return `Coverage: ${coverage.extracted_lines}/${coverage.input_lines} lines extracted (${coverage.no_signal_lines} no-signal, ${coverage.empty_lines} empty)`;
}

export interface CheckpointEntry {
  history_id: number;
  memory_id: string;
  operation: string;
  changed_at: string;
}

export function checkpointLabel(entry: CheckpointEntry): string {
  return `${entry.operation} · ${entry.changed_at}`;
}

export function formatStatsMessage(stats: DashboardStats): string {
  return [
    `Total: ${stats.total}`,
    `MUST: ${stats.must_count} | REF: ${stats.reference_count}`,
    `L3: ${stats.layers.l3} | L2: ${stats.layers.l2} | L1: ${stats.layers.l1} | L0: ${stats.layers.l0}`,
    `Skills: ${stats.skills} | Reviewed: ${stats.reviewed_count}`,
    `Agents: ${stats.agents.length} | Namespaces: ${stats.namespaces.length}`,
  ].join(' · ');
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
