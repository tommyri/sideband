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
import { TestingPanel } from './features/testing/TestingPanel'
import { VoiceDock } from './features/voice/VoiceDock'

export default function App() {
  const hydrate = useAppStore((state) => state.hydrate)
  const theme = useAppStore((state) => state.theme)
  const server = useAppStore((state) => state.servers.find((s) => s.id === state.selectedServerId))
  const [startupError, setStartupError] = useState('')

  useEffect(() => {
    void subscribeToClientEvents()
      .then(() => getSnapshot())
      .then((snapshot) => hydrate(snapshot.servers))
      .catch(() =>
        setStartupError(
          'Could not load saved servers. Restart the app and check that its data directory is writable.',
        ),
      )
  }, [hydrate])

  return (
    <div className="app-shell" data-theme={theme}>
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
      <DebugPanel />
      <ConnectDialog />
      {isTauri() && (
        <>
          <ShortcutGuard />
          <TestingPanel />
          <DiagnosticsPanel />
          <VoiceDock />
        </>
      )}
    </div>
  )
}
