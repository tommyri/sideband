import { create } from 'zustand'
import type { AppEvent, ConnectResult, ServerSummary } from '../lib/bindings'

type Theme = 'dark' | 'light'

interface AppStore {
  servers: ServerSummary[]
  selectedServerId: string | null
  events: AppEvent[]
  debugOpen: boolean
  connectOpen: boolean
  connectServer: ServerSummary | null
  theme: Theme
  hydrate: (servers: ServerSummary[]) => void
  selectServer: (id: string) => void
  addConnection: (
    result: ConnectResult,
    address: { host: string; port: number; httpsPort: number },
  ) => void
  receiveEvent: (event: AppEvent) => void
  toggleDebug: () => void
  setConnectOpen: (open: boolean, server?: ServerSummary) => void
  toggleTheme: () => void
}

export const useAppStore = create<AppStore>((set) => ({
  servers: [],
  selectedServerId: null,
  events: [],
  debugOpen: true,
  connectOpen: false,
  connectServer: null,
  theme: 'dark',
  hydrate: (servers) =>
    set((state) => ({
      servers,
      selectedServerId: servers.some((s) => s.id === state.selectedServerId)
        ? state.selectedServerId
        : (servers[0]?.id ?? null),
    })),
  selectServer: (selectedServerId) => set({ selectedServerId }),
  addConnection: (result, address) =>
    set((state) => {
      const updated = {
        id: result.serverId,
        label: result.label,
        ...address,
        unread: 0,
        status: result.status,
      }
      return {
        selectedServerId: result.serverId,
        servers: state.servers.some((s) => s.id === result.serverId)
          ? state.servers.map((s) => (s.id === result.serverId ? updated : s))
          : [...state.servers, updated],
        connectOpen: false,
      }
    }),
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
  setConnectOpen: (connectOpen, server) => set({ connectOpen, connectServer: server ?? null }),
  toggleTheme: () => set((state) => ({ theme: state.theme === 'dark' ? 'light' : 'dark' })),
}))
