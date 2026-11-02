import { create } from 'zustand'
import type { ServerView } from '../../lib/generated/ServerView'
import type { ActiveVoiceView } from '../../lib/generated/ActiveVoiceView'
import type { VoiceStatsPayload } from '../../lib/bindings'
import { useAppStore } from '../../store/app-store'
import { getSnapshot } from '../../lib/api'
import type { ApiError } from '../../lib/bindings'
import { getServerView } from './api'

interface VoiceStore {
  views: Record<string, ServerView>
  active: ActiveVoiceView | null
  speaking: Record<string, string[]>
  stats: Record<string, VoiceStatsPayload>
  selected: Record<string, string>
  busy: boolean
  error: string
  deviceNotice: string
  select: (server: string, channel: string) => void
}
export const useVoiceStore = create<VoiceStore>((set) => ({
  views: {},
  active: null,
  speaking: {},
  stats: {},
  selected: {},
  busy: false,
  error: '',
  deviceNotice: '',
  select: (server, channel) => set((s) => ({ selected: { ...s.selected, [server]: channel } })),
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
export async function refreshVoice(server: string) {
  const revision = (revisions[server] ?? 0) + 1
  revisions[server] = revision
  try {
    const view = await getServerView(server)
    if (revisions[server] !== revision) return
    useVoiceStore.setState((s) => ({
      active: view.activeVoice,
      views: {
        ...s.views,
        [server]: {
          ...view,
          channels: reuse(s.views[server]?.channels, view.channels),
          members: reuse(s.views[server]?.members, view.members),
        },
      },
    }))
  } catch {
    if (revisions[server] !== revision) return
    useVoiceStore.setState((s) => {
      const views = { ...s.views }
      delete views[server]
      return { views, active: s.active?.serverId === server ? null : s.active }
    })
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
export async function runVoice(action: () => Promise<void>) {
  if (useVoiceStore.getState().busy) return
  useVoiceStore.setState({ busy: true, error: '' })
  try {
    await action()
  } catch (error) {
    const failure = error as Partial<ApiError>
    useVoiceStore.setState({
      error:
        failure.code === 'server_2'
          ? 'You do not have permission for that action. Your roles or channel permissions may have changed.'
          : failure.code === 'disconnected'
            ? 'The server is disconnected. Reconnect before trying again.'
            : (failure.message ?? 'The voice action failed. Please try again.'),
    })
  } finally {
    await Promise.all(useAppStore.getState().servers.map((s) => refreshVoice(s.id)))
    try {
      useAppStore.getState().hydrate((await getSnapshot()).servers)
    } catch {
      /* connection event will retry */
    }
    useVoiceStore.setState({ busy: false })
  }
}
