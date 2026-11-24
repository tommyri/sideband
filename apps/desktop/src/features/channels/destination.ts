import type { ServerView } from '../../lib/generated/ServerView'

type Conversation = { id: string; kind: 'channel' | 'dm' }

const serverPrefix = (server: string) => `sideband.conversation.${encodeURIComponent(server)}/`
const key = (server: string, user: string) => serverPrefix(server) + encodeURIComponent(user)

export function lastConversation(server: string, user: string): Conversation | undefined {
  try {
    const saved = JSON.parse(localStorage.getItem(key(server, user)) ?? 'null')
    if (typeof saved?.id === 'string' && (saved.kind === 'channel' || saved.kind === 'dm'))
      return saved
  } catch {
    // Storage may be unavailable or contain an older preference.
  }
}

export function rememberConversation(server: string, user: string, conversation: Conversation) {
  try {
    localStorage.setItem(key(server, user), JSON.stringify(conversation))
  } catch {
    // In-memory navigation still works when storage is unavailable.
  }
}

export function forgetConversations(server: string) {
  try {
    for (const entry of Object.keys(localStorage))
      if (entry.startsWith(serverPrefix(server))) localStorage.removeItem(entry)
  } catch {
    // Storage is optional.
  }
}

// The server supplies channel-tree order and filters channels by visibility and DMs by membership.
export function hasDestination(view: ServerView, id: string | undefined) {
  return (
    !!id &&
    (view.channels.some((c) => c.id === id && (c.kind === 2 || c.kind === 3)) ||
      view.directMessages.some((dm) => dm.id === id))
  )
}

export function workspaceDestination(
  view: ServerView,
  server: string,
  selected: string | undefined,
  saved: Conversation | undefined,
) {
  if (selected && hasDestination(view, selected)) return selected

  const text = view.channels.filter((c) => c.kind === 2)
  const hasVoice = view.channels.some((c) => c.kind === 3)
  if (selected === '' && !text.length && hasVoice) return ''
  if (saved?.kind === 'dm' && view.directMessages.some((dm) => dm.id === saved.id)) return saved.id
  if (saved?.kind === 'channel' && text.some((c) => c.id === saved.id)) return saved.id
  const first = text.find((c) => c.name.toLowerCase() === 'general') ?? text[0]
  if (first) return first.id
  if (hasVoice) {
    const active = view.activeVoice
    if (
      selected === undefined &&
      active?.serverId === server &&
      view.channels.some((c) => c.id === active.channelId && c.kind === 3)
    )
      return active.channelId
    return ''
  }
  return ''
}
