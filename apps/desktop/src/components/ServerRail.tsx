import { useState } from 'react'
import { NotificationSettings } from '../features/notifications/NotificationSettings'
import { Bell, Plus } from 'lucide-react'
import { useAppStore } from '../store/app-store'

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

  return (
    <nav className="server-rail" aria-label="Servers">
      <div className="brand-mark" aria-label="Sideband">
        <img src="/brand/sideband-mark.svg" width={32} height={32} alt="" />
      </div>
      <div className="rail-rule" />
      <div className="server-buttons">
        {servers.map((server, index) => (
          <button
            key={server.id}
            className={`server-button ${selected === server.id ? 'active' : ''}`}
            onClick={() => select(server.id)}
            aria-label={server.label}
            title={`${server.label} · ${server.status.kind}`}
          >
            {index === 0 ? <span className="server-grid">▦</span> : initials(server.label)}
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
      {notifications && <NotificationSettings close={() => setNotifications(false)} />}
      <div className="rail-avatar" aria-label="Your profile">
        TR
        <span className="presence-dot" />
      </div>
    </nav>
  )
}
