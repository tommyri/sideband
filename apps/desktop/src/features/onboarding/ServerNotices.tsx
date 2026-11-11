import { useServerNotices } from '../../lib/notices'

const none: never[] = []
// What the server or the client said about this server: removals, notices
// such as a failed voice rejoin, and unsolicited errors.
export function ServerNotices({ server }: { server: string }) {
  const notices = useServerNotices((s) => s.notices[server] ?? none)
  const dismiss = useServerNotices((s) => s.dismiss)
  if (!notices.length) return null
  return (
    <ul aria-label="Server notices" style={{ listStyle: 'none', padding: 0 }}>
      {notices.map((notice) => (
        <li key={notice.id} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <p
            role={notice.tone}
            className={notice.tone === 'alert' ? 'form-error' : 'connection-banner'}
          >
            {notice.text}
          </p>
          <button className="button secondary" onClick={() => dismiss(server, notice.id)}>
            Dismiss
          </button>
        </li>
      ))}
    </ul>
  )
}
