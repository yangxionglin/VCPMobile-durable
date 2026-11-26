import type { ContentBlock } from '../../../core/types/chat';

type HistoryToolProcess = {
  start: number;
  end: number;
  count: number;
  failed: number;
  waiting: boolean;
};

function isTool(block: ContentBlock) {
  return block.type === 'tool-use' || block.type === 'jev-tool-use' || block.type === 'tool-result';
}

function isResultWrapper(block: ContentBlock | undefined, isEnd: boolean) {
  return block?.type === 'role-divider' && block.role.toLowerCase() === 'user' && block.is_end === isEnd;
}

/** Mirror VCPChat's completed-turn tool fold on the already parsed message blocks. */
export function getHistoryToolProcess(blocks: ContentBlock[]): HistoryToolProcess | undefined {
  let first = -1;
  let last = -1;
  let count = 0;
  let hasReceipt = false;
  let failed = 0;
  let waiting = false;

  for (const [index, block] of blocks.entries()) {
    if (!isTool(block)) continue;
    if (first < 0) first = index;
    last = index;

    if (block.type !== 'tool-result') {
      count++;
      continue;
    }
    hasReceipt = true;
    // A named consultation reply stays a separate part of the conversation.
    if (block.tool_name === 'AgentAssistant' && block.details.some(detail => detail.key === '回复成员' && detail.value.trim())) return;

    // The desktop only counts an adjacent same-name request and result once.
    const previous = blocks[index - 1];
    if ((previous?.type !== 'tool-use' && previous?.type !== 'jev-tool-use') || previous.tool_name !== block.tool_name) count++;

    const status = block.status.trim().replace(/^(?:\s|✅|❌|⚠️?|⏳)+/u, '').toLowerCase();
    if (['failed', 'failure', 'error', '失败', '错误'].includes(status)) failed++;
    if (['pendingapproval', 'pending_approval', '待确认'].includes(status)) waiting = true;
  }

  if (!hasReceipt || count < 2) return;

  // Synthetic user delimiters around receipts belong inside the fold. Leave
  // the leading prose and the answer following the last receipt in place.
  const start = isResultWrapper(blocks[first - 1], false) ? first - 1 : first;
  const lastReceiptBlock = blocks[last + 1]?.type === 'tool-call-summary' ? last + 1 : last;
  const end = isResultWrapper(blocks[lastReceiptBlock + 1], true) ? lastReceiptBlock + 1 : lastReceiptBlock;
  for (let index = start; index <= end; index++) {
    const block = blocks[index];
    if (block.type === 'voice-guidance' || block.type === 'button-click' || block.type === 'html-preview'
      || (block.type === 'role-divider' && block.role.toLowerCase() !== 'user')) return;
  }
  return { start, end, count, failed, waiting };
}
