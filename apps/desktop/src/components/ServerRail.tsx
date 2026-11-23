import { TestingPanel } from '../features/testing/TestingPanel'
import { UpdateControl } from '../features/updates/UpdateControl'
import { isTauri } from '../lib/tauri'
import { useState } from 'react'
import { NotificationSettings } from '../features/notifications/NotificationSettings'
import { Bell, Bug, Plus } from 'lucide-react'
import { useAppStore } from '../store/app-store'
import { useVoiceStore } from '../features/voice/store'

function initials(label: string) {
  return label
    .split(/\s+/)
    .map((word) => word[0])
    .join('')
    .slice(0, 2)
    .toUpperCase()
}

export function ServerRail() {
  const [notifications, setNotifications] = useState(false)
  const servers = useAppStore((state) => state.servers)
  const selected = useAppStore((state) => state.selectedServerId)
  const select = useAppStore((state) => state.selectServer)
  const openConnect = useAppStore((state) => state.setConnectOpen)
  const toggleDebug = useAppStore((state) => state.toggleDebug)
  const own = useVoiceStore((state) => {
    const view = selected ? state.views[selected] : undefined
    return view?.members.find((member) => member.id === view.ownUserId)
  })

  return (
    <nav className="server-rail" aria-label="Servers">
      <div className="brand-mark" aria-label="Sideband">
        <img src="/brand/sideband-mark.svg" width={32} height={32} alt="" />
      </div>
      <div className="rail-rule" />
      <div className="server-buttons">
        {servers.map((server) => (
          <button
            key={server.id}
            className={`server-button ${selected === server.id ? 'active' : ''}`}
            onClick={() => select(server.id)}
            aria-label={server.label}
            title={`${server.label} · ${server.status.kind}`}
          >
            {initials(server.label)}
            <span className={`status-dot ${server.status.kind}`} />
            {server.unread > 0 && (
              <span className="rail-unread" aria-label={`${server.unread} unread channels`}>
                {server.unread}
              </span>
            )}
          </button>
        ))}
        <button
          className="server-button add-server"
          onClick={(event) => {
            event.currentTarget.focus()
            openConnect(true)
          }}
          aria-label="Add server"
        >
          <Plus size={20} />
        </button>
      </div>
      <button
        className="server-button"
        aria-label="Notification settings"
        onClick={() => setNotifications(true)}
      >
        <Bell size={20} />
      </button>
      {isTauri() && (
        <>
          <UpdateControl />
          <TestingPanel />
        </>
      )}
      {import.meta.env.DEV && (
        <button
          className="server-button"
          aria-label="Toggle event inspector"
          title="Event inspector (development builds only)"
          onClick={toggleDebug}
        >
          <Bug size={20} />
        </button>
      )}
      {notifications && <NotificationSettings close={() => setNotifications(false)} />}
      {own && (
        <div
          className="rail-avatar"
          role="img"
          aria-label={`Signed in as ${own.displayName || own.username}, ${own.presence}`}
          title={`${own.displayName || own.username} · ${own.presence}`}
        >
          {initials(own.displayName || own.username)}
          {own.presence === 'online' && <span className="presence-dot" />}
        </div>
      )}
    </nav>
  )
}
