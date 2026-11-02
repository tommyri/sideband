import { create } from 'zustand'
import type { AppEvent, ConnectResult, ServerSummary } from '../lib/bindings'

type Theme = 'dark' | 'light'

interface AppStore {
  servers: ServerSummary[]
  selectedServerId: string | null
  events: AppEvent[]
  debugOpen: boolean
  connectOpen: boolean
  theme: Theme
  hydrate: (servers: ServerSummary[]) => void
  selectServer: (id: string) => void
  addConnection: (result: ConnectResult) => void
  receiveEvent: (event: AppEvent) => void
  toggleDebug: () => void
  setConnectOpen: (open: boolean) => void
  toggleTheme: () => void
}

export const useAppStore = create<AppStore>((set) => ({
  servers: [],
  selectedServerId: null,
  events: [],
  debugOpen: true,
  connectOpen: false,
  theme: 'dark',
  hydrate: (servers) =>
    set((state) => ({
      servers,
      selectedServerId: state.selectedServerId ?? servers[0]?.id ?? null,
    })),
  selectServer: (selectedServerId) => set({ selectedServerId }),
  addConnection: (result) =>
    set((state) => ({
      selectedServerId: result.serverId,
      servers: [
        ...state.servers.filter((server) => server.id !== result.serverId),
        {
          id: result.serverId,
          label: result.label,
          host: result.serverId.split(':')[0] ?? result.serverId,
          status: result.status,
        },
      ],
      connectOpen: false,
    })),
  receiveEvent: (incoming) =>
    set((state) => {
      const connectionStatus =
        incoming.event.kind === 'connection' ? incoming.event.status : undefined
      return {
        events: [incoming, ...state.events].slice(0, 100),
        servers: connectionStatus
          ? state.servers.map((server) =>
              server.id === incoming.serverId ? { ...server, status: connectionStatus } : server,
            )
          : state.servers,
      }
    }),
  toggleDebug: () => set((state) => ({ debugOpen: !state.debugOpen })),
  setConnectOpen: (connectOpen) => set({ connectOpen }),
  toggleTheme: () => set((state) => ({ theme: state.theme === 'dark' ? 'light' : 'dark' })),
}))
