import { useEffect } from 'react'
import { ChannelSidebar } from './components/ChannelSidebar'
import { ConnectDialog } from './components/ConnectDialog'
import { Conversation } from './components/Conversation'
import { DebugPanel } from './components/DebugPanel'
import { MemberList } from './components/MemberList'
import { ServerRail } from './components/ServerRail'
import { getSnapshot } from './lib/api'
import { subscribeToClientEvents } from './lib/events'
import { useAppStore } from './store/app-store'

export default function App() {
  const hydrate = useAppStore((state) => state.hydrate)
  const theme = useAppStore((state) => state.theme)

  useEffect(() => {
    void getSnapshot().then((snapshot) => hydrate(snapshot.servers))
    void subscribeToClientEvents()
  }, [hydrate])

  return (
    <div className="app-shell" data-theme={theme}>
      <ServerRail />
      <ChannelSidebar />
      <Conversation />
      <MemberList />
      <DebugPanel />
      <ConnectDialog />
    </div>
  )
}
