import { useEffect, useState } from 'react'
import { ChannelSidebar } from './components/ChannelSidebar'
import { ConnectDialog } from './components/ConnectDialog'
import { Conversation } from './components/Conversation'
import { DebugPanel } from './components/DebugPanel'
import { MemberList } from './components/MemberList'
import { ServerRail } from './components/ServerRail'
import { getSnapshot } from './lib/api'
import { subscribeToClientEvents } from './lib/events'
import { useAppStore } from './store/app-store'
import { isTauri } from './lib/tauri'
import { ServerHome } from './features/onboarding/ServerHome'
import { ChannelWorkspace } from './features/channels/ChannelWorkspace'
import { ShortcutGuard } from './features/settings/shortcuts/ShortcutGuard'
import { DiagnosticsPanel } from './features/diagnostics/DiagnosticsPanel'
import { VoiceDock } from './features/voice/VoiceDock'

export default function App() {
  const hydrate = useAppStore((state) => state.hydrate)
  const theme = useAppStore((state) => state.theme)
  const server = useAppStore((state) => state.servers.find((s) => s.id === state.selectedServerId))
  const [startupError, setStartupError] = useState('')

  useEffect(() => {
    let live = true
    void subscribeToClientEvents()
      .then(() => getSnapshot())
      .then((snapshot) => {
        if (!live) return
        hydrate(snapshot.servers)
        if (isTauri() && !snapshot.servers.length) useAppStore.getState().setConnectOpen(true)
      })
      .catch(() => {
        if (live)
          setStartupError(
            'Could not load saved servers. Restart the app and check that its data directory is writable.',
          )
      })
    return () => {
      live = false
    }
  }, [hydrate])

  return (
    <div className="app-shell" data-theme={theme}>
      <div className="app-workspace">
        <ServerRail />
        {isTauri() ? (
          server && ['connected', 'reconnecting'].includes(server.status.kind) ? (
            <ChannelWorkspace key={server.id} server={server.id} />
          ) : (
            <ServerHome />
          )
        ) : (
          <>
            <ChannelSidebar />
            <Conversation />
            <MemberList />
          </>
        )}
        {startupError && (
          <p role="alert" className="startup-error">
            {startupError}
          </p>
        )}
        {/* Development builds only: Vite drops the inspector from packaged builds. */}
        {import.meta.env.DEV && <DebugPanel />}
      </div>
      <ConnectDialog />
      {isTauri() && (
        <>
          <ShortcutGuard />
          <DiagnosticsPanel />
          <VoiceDock />
        </>
      )}
    </div>
  )
}
