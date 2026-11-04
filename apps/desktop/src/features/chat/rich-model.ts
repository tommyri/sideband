export type ChatMember = { id: string; username: string; displayName: string }
export const emoji: Record<string, string> = {
  smile: '😄',
  grin: '😁',
  joy: '😂',
  slightly_smiling_face: '🙂',
  wink: '😉',
  heart: '❤️',
  heart_eyes: '😍',
  thinking: '🤔',
  eyes: '👀',
  cry: '😢',
  sob: '😭',
  angry: '😠',
  scream: '😱',
  sunglasses: '😎',
  tada: '🎉',
  party: '🥳',
  thumbsup: '👍',
  thumbsdown: '👎',
  wave: '👋',
  clap: '👏',
  pray: '🙏',
  fire: '🔥',
  rocket: '🚀',
  check: '✅',
  x: '❌',
  warning: '⚠️',
  gaming: '🎮',
  headphones: '🎧',
  coffee: '☕',
  '+1': '👍',
  '-1': '👎',
}
export function canonicalId(value: string): string | null {
  const normalized = value.toUpperCase().replace(/[IL]/g, '1').replace(/O/g, '0')
  return /^[0-7][0-9A-HJKMNP-TV-Z]{25}$/.test(normalized) ? normalized : null
}
export function messageLink(server: string, channel: string, message: string) {
  return `sideband://message/${encodeURIComponent(server)}/${channel}/${message}`
}
export function parseMessageLink(value: string) {
  try {
    const url = new URL(value)
    const parts = url.pathname.split('/').slice(1)
    if (
      url.protocol !== 'sideband:' ||
      url.host !== 'message' ||
      parts.length !== 3 ||
      url.search ||
      url.hash ||
      url.username ||
      url.password
    )
      return null
    const server = decodeURIComponent(parts[0]!)
    const channel = canonicalId(parts[1]!)
    const message = canonicalId(parts[2]!)
    return server && channel && message ? { server, channel, message } : null
  } catch {
    return null
  }
}
export function safeLink(value: string): string {
  if (parseMessageLink(value)) return value
  try {
    const url = new URL(value)
    return ['https:', 'http:'].includes(url.protocol) &&
      url.hostname &&
      !url.username &&
      !url.password
      ? url.href
      : ''
  } catch {
    return ''
  }
}
export function completionAt(value: string, cursor: number) {
  const match = /(?:^|\s)([@:])([^\s@:<>]*)$/.exec(value.slice(0, cursor))
  return match
    ? {
        kind: match[1]!,
        query: match[2]!.toLocaleLowerCase(),
        start: cursor - match[2]!.length - 1,
        end: cursor,
      }
    : null
}
export function completeOptions(kind: string, query: string, members: ChatMember[]) {
  return kind === '@'
    ? members
        .filter((m) => `${m.username} ${m.displayName}`.toLocaleLowerCase().includes(query))
        .slice(0, 8)
        .map((m) => ({
          label: `${m.displayName || m.username} @${m.username}`,
          value: `<@${m.id}> `,
        }))
    : Object.entries(emoji)
        .filter(([name]) => name.startsWith(query))
        .slice(0, 8)
        .map(([name, value]) => ({ label: `${value} :${name}:`, value: `${value} ` }))
}
export function wrapSelection(value: string, start: number, end: number, marker: string) {
  return {
    value: value.slice(0, start) + marker + value.slice(start, end) + marker + value.slice(end),
    start: start + marker.length,
    end: end + marker.length,
  }
}
