/** Rendering for terminal and JSON output. */

import type { Chat, Message } from './db.js';

const TIME = new Intl.DateTimeFormat(undefined, { hour: 'numeric', minute: '2-digit' });
const DATE_TIME = new Intl.DateTimeFormat(undefined, {
  month: 'short',
  day: 'numeric',
  hour: 'numeric',
  minute: '2-digit',
});

function isToday(date: Date): boolean {
  const now = new Date();
  return (
    date.getFullYear() === now.getFullYear() &&
    date.getMonth() === now.getMonth() &&
    date.getDate() === now.getDate()
  );
}

export function formatTimestamp(date: Date | null): string {
  if (date === null) return '';
  return isToday(date) ? TIME.format(date) : DATE_TIME.format(date);
}

export function relativeAge(date: Date | null): string {
  if (date === null) return 'never';
  const seconds = Math.max(0, (Date.now() - date.getTime()) / 1000);
  if (seconds < 60) return 'just now';
  if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
  if (seconds < 86_400) return `${Math.floor(seconds / 3600)}h ago`;
  if (seconds < 2_592_000) return `${Math.floor(seconds / 86_400)}d ago`;
  return DATE_TIME.format(date);
}

export function renderChats(chats: Chat[]): string {
  if (chats.length === 0) return 'no chats found\n';
  const width = Math.min(38, Math.max(...chats.map((c) => c.name.length)));
  const ages = chats.map((c) => relativeAge(c.lastDate));
  const ageWidth = Math.max(...ages.map((a) => a.length));
  return `${chats
    .map((chat, index) => {
      const id = String(chat.rowid).padStart(5);
      const name = truncate(chat.name, width).padEnd(width);
      const kind = chat.isGroup ? `${chat.memberCount} people` : 'direct';
      return `${id}  ${name}  ${(ages[index] ?? '').padEnd(ageWidth)}  ${kind}`;
    })
    .join('\n')}\n`;
}

export function renderMessages(messages: Message[], showChat = false): string {
  if (messages.length === 0) return 'no messages found\n';
  return `${messages
    .map((message) => {
      const stamp = formatTimestamp(message.date);
      const where = showChat && message.chatName !== null ? `[${message.chatName}] ` : '';
      const body = message.body ?? '(no text)';
      return `${stamp}  ${where}${message.sender}: ${body}`;
    })
    .join('\n')}\n`;
}

function truncate(value: string, width: number): string {
  return value.length <= width ? value : `${value.slice(0, width - 1)}…`;
}

export function toJson(value: unknown): string {
  return `${JSON.stringify(value, null, 2)}\n`;
}
