import { useState } from 'react'
import { Activity, CircleCheck, CircleOff, Copy, Radio, Trash2, X } from 'lucide-react'
import type { AppEvent } from '../lib/bindings'
import { useAppStore } from '../store/app-store'
import { isTauri } from '../lib/tauri'

/** Development-only view of the `ss://event` stream. App renders it only when
 * `import.meta.env.DEV`, so packaged builds never include it. */

const previewEvents: AppEvent[] = [
  {
    serverId: 'preview.local:5781',
    occurredAtMs: Date.now() - 4_000,
    event: { kind: 'messagesChanged', channel: 'general' },
  },
  {
    serverId: 'preview.local:5781',
    occurredAtMs: Date.now() - 12_000,
    event: { kind: 'voiceChanged' },
  },
  {
    serverId: 'preview.local:5781',
    occurredAtMs: Date.now() - 19_000,
    event: { kind: 'membersChanged' },
  },
  {
    serverId: 'preview.local:5781',
    occurredAtMs: Date.now() - 32_000,
    event: { kind: 'connection', status: { kind: 'connected' } },
  },
]

function eventDetail(item: AppEvent) {
  switch (item.event.kind) {
    case 'messagesChanged':
      return `channel: ${item.event.channel}`
    case 'connection':
      return `status: ${item.event.status.kind}`
    default:
      return `server: ${item.serverId}`
  }
}

export function DebugPanel() {
  const open = useAppStore((state) => state.debugOpen)
  const toggle = useAppStore((state) => state.toggleDebug)
  const events = useAppStore((state) => state.events)
  const clear = useAppStore((state) => state.clearEvents)
  const [notice, setNotice] = useState('')
  const native = isTauri()
  const visibleEvents = events.length || native ? events : previewEvents
  const copy = () =>
    void navigator.clipboard.writeText(JSON.stringify(visibleEvents, null, 2)).then(
      () => setNotice('Copied'),
      () => setNotice('Copy failed'),
    )

  if (!open) return null

  return (
    <aside className="debug-panel">
      <header>
        <div>
          <p className="eyebrow">Native bridge</p>
          <strong>Event inspector</strong>
        </div>
        <button className="icon-button" onClick={toggle} aria-label="Close debug panel">
          <X size={17} />
        </button>
      </header>

      <div className="bridge-health">
        <div className="bridge-health-icon">
          <Radio size={18} />
        </div>
        <span>
          <strong>{native ? 'Native bridge' : 'Browser preview'}</strong>
          <small>{native ? 'Listening on ss://event' : 'No native bridge; sample events'}</small>
        </span>
        <span className="health-pill">
          {native ? <CircleCheck size={13} /> : <CircleOff size={13} />}{' '}
          {native ? `${events.length} received` : 'Preview'}
        </span>
      </div>

      <div className="debug-toolbar">
        <span>
          <Activity size={15} /> Recent events
        </span>
        <div>
          {notice && <small role="status">{notice}</small>}
          <button aria-label="Copy event log" onClick={copy}>
            <Copy size={14} />
          </button>
          <button
            aria-label="Clear event log"
            onClick={() => {
              clear()
              setNotice('')
            }}
          >
            <Trash2 size={14} />
          </button>
        </div>
      </div>

      <div className="event-list">
        {visibleEvents.map((item, index) => (
          <article className="event-card" key={`${item.occurredAtMs}-${index}`}>
            <div>
              <span className="event-pulse" />
              <strong>{item.event.kind}</strong>
              <time>
                {new Date(item.occurredAtMs).toLocaleTimeString([], {
                  hour: '2-digit',
                  minute: '2-digit',
                  second: '2-digit',
                })}
              </time>
            </div>
            <code>{eventDetail(item)}</code>
          </article>
        ))}
      </div>

      <footer>
        <span>Showing {visibleEvents.length} events</span>
        <span className="debug-dot" />
        <span>bounded buffer</span>
      </footer>
    </aside>
  )
}
