import { create } from 'zustand'
import type { ServerView } from '../../lib/generated/ServerView'
import type { ActiveVoiceView } from '../../lib/generated/ActiveVoiceView'
import type { VoiceStatsPayload } from '../../lib/bindings'
import { useAppStore } from '../../store/app-store'
import { getSnapshot, loadDmPage } from '../../lib/api'
import { hasDestination, lastConversation, workspaceDestination } from '../channels/destination'
import type { ApiError } from '../../lib/bindings'
import { getServerView } from './api'

interface VoiceStore {
  views: Record<string, ServerView>
  active: ActiveVoiceView | null
  speaking: Record<string, string[]>
  stats: Record<string, VoiceStatsPayload>
  selected: Record<string, string>
  busy: boolean
  joining: Pick<ActiveVoiceView, 'serverId' | 'channelId'> | null
  error: string
  deviceNotice: string
  deviceRoute: string
  select: (server: string, channel: string) => void
}
// A DM creation or message link can arrive before its channel event. Keep
// that explicit request until the account's view includes it or the user moves on.
const requested = new Map<string, { id: string; user: string }>()

export const useVoiceStore = create<VoiceStore>((set) => ({
  views: {},
  active: null,
  speaking: {},
  stats: {},
  selected: {},
  busy: false,
  joining: null,
  error: '',
  deviceNotice: '',
  deviceRoute: '',
  select: (server, channel) =>
    set((s) => {
      const view = s.views[server]
      if (view && channel && !hasDestination(view, channel))
        requested.set(server, { id: channel, user: view.ownUserId })
      else requested.delete(server)
      return { selected: { ...s.selected, [server]: channel } }
    }),
}))

// Keep unchanged row objects stable when a membership or channel event arrives.
function reuse<T extends { id: string }>(old: T[] | undefined, next: T[]): T[] {
  const previous = new Map(old?.map((item) => [item.id, item]))
  return next.map((item) => {
    const before = previous.get(item.id)
    return before && JSON.stringify(before) === JSON.stringify(item) ? before : item
  })
}
const revisions: Record<string, number> = {}

function currentSelection(server: string, view: ServerView) {
  const { selected, views } = useVoiceStore.getState()
  return !views[server] || views[server].ownUserId === view.ownUserId ? selected[server] : undefined
}

function discardServerView(server: string) {
  useVoiceStore.setState((s) => {
    const views = { ...s.views }
    const selected = { ...s.selected }
    delete views[server]
    delete selected[server]
    return { views, selected }
  })
}

export function forgetServerView(server: string) {
  revisions[server] = (revisions[server] ?? 0) + 1
  requested.delete(server)
  discardServerView(server)
}

export async function refreshVoice(server: string) {
  const revision = (revisions[server] ?? 0) + 1
  revisions[server] = revision
  try {
    let view = await getServerView(server)
    if (revisions[server] !== revision) return
    let saved = lastConversation(server, view.ownUserId)
    // Welcome contains only the newest DMs. Validate an older saved DM against
    // the account's paged membership list before opening any history.
    while (
      saved?.kind === 'dm' &&
      view.moreDirectMessages &&
      !view.directMessages.some((dm) => dm.id === saved?.id) &&
      !hasDestination(view, currentSelection(server, view))
    ) {
      // A resync can remove the open DM. Hide the previous account/room
      // immediately while paging, including when a page fails and needs a retry.
      if (useVoiceStore.getState().views[server]) discardServerView(server)
      const more = await loadDmPage(server)
      if (revisions[server] !== revision) return
      const next = await getServerView(server)
      if (revisions[server] !== revision) return
      view = next
      saved = lastConversation(server, view.ownUserId)
      if (!more) break
    }
    if (revisions[server] !== revision) return
    const pending = requested.get(server)
    const ready = pending?.user === view.ownUserId && hasDestination(view, pending.id)
    if (ready || pending?.user !== view.ownUserId) requested.delete(server)
    const selection = workspaceDestination(
      view,
      server,
      ready ? pending.id : currentSelection(server, view),
      saved,
    )
    useVoiceStore.setState((s) => ({
      active: view.activeVoice,
      selected:
        s.selected[server] === selection ? s.selected : { ...s.selected, [server]: selection },
      views: {
        ...s.views,
        [server]: {
          ...view,
          channels: reuse(s.views[server]?.channels, view.channels),
          members: reuse(s.views[server]?.members, view.members),
        },
      },
    }))
  } catch (error) {
    if (revisions[server] !== revision) return
    // The shell ends a voice session before it drops the server's client, so
    // only `disconnected` means the session is gone. Any other failure keeps
    // the dock (mute, leave, stop sharing) while the native session runs.
    const gone = (error as Partial<ApiError> | null)?.code === 'disconnected'
    forgetServerView(server)
    if (gone && useVoiceStore.getState().active?.serverId === server)
      useVoiceStore.setState({ active: null })
  }
}
const timers = new Map<string, ReturnType<typeof setTimeout>>()
export function scheduleVoiceRefresh(server: string) {
  if (timers.has(server)) return
  timers.set(
    server,
    setTimeout(() => {
      timers.delete(server)
      void refreshVoice(server)
    }, 60),
  )
}
export async function runVoice(action: () => Promise<void>, joining: VoiceStore['joining'] = null) {
  if (useVoiceStore.getState().busy) return
  useVoiceStore.setState({ busy: true, joining, error: '' })
  try {
    await action()
  } catch (error) {
    const failure = error as Partial<ApiError>
    const reason =
      failure.code === 'server_2'
        ? 'You do not have permission for that action. Your roles or channel permissions may have changed.'
        : failure.code === 'disconnected'
          ? 'The server is disconnected. Reconnect before trying again.'
          : (failure.message ?? 'The voice action failed. Please try again.')
    const room =
      joining &&
      useVoiceStore
        .getState()
        .views[joining.serverId]?.channels.find((c) => c.id === joining.channelId)
    useVoiceStore.setState({ error: room ? `Could not join ${room.name}. ${reason}` : reason })
  } finally {
    await Promise.all(useAppStore.getState().servers.map((s) => refreshVoice(s.id)))
    try {
      useAppStore.getState().hydrate((await getSnapshot()).servers)
    } catch {
      /* connection event will retry */
    }
    useVoiceStore.setState({ busy: false, joining: null })
  }
}
