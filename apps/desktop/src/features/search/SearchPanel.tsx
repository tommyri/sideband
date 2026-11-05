import { useEffect, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import type { SearchQuery } from '../../lib/generated/SearchQuery'
import type { SearchResult } from '../../lib/generated/SearchResult'
import { invokeBridge } from '../../lib/tauri'
import { errorText, onChatEvent } from '../chat/api'
import { navigateMessage } from '../chat/navigation'
import { messageLink } from '../chat/rich-model'
import { dmName } from '../dm/model'
import './search.css'

export function SearchPanel({
  server,
  view,
  close,
}: {
  server: string
  view: ServerView
  close: () => void
}) {
  const dialog = useRef<HTMLDialogElement>(null)
  const generation = useRef(0)
  const [q, setQ] = useState('')
  const [channel, setChannel] = useState('')
  const [author, setAuthor] = useState('')
  const [before, setBefore] = useState('')
  const [after, setAfter] = useState('')
  const [has, setHas] = useState('')
  const [rows, setRows] = useState<SearchResult[]>([])
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('Search words are matched together. Dates use UTC.')
  const [error, setError] = useState('')
  const members = new Map(view.members.map((m) => [m.id, m]))
  const channels = new Map([
    ...view.channels.filter((c) => c.kind === 2).map((c) => [c.id, `#${c.name}`] as const),
    ...view.directMessages.map((d) => [d.id, dmName(d, members, view.ownUserId)] as const),
  ])
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    const requests = generation
    const stop = onChatEvent(({ serverId, event }) => {
      if (
        serverId !== server ||
        ![
          'resynced',
          'membersChanged',

          'channelsChanged',
          'messagesChanged',
          'connection',
          'removed',
        ].includes(event.kind)
      )
        return
      requests.current++
      setRows([])
      setBusy(false)
      setNotice('Messages or access changed. Search again for current results.')
    })
    return () => {
      requests.current++
      stop()
      element?.close()
      previous?.focus()
    }
  }, [server])
  async function search() {
    const revision = ++generation.current
    setRows([])
    setBusy(true)
    setError('')
    setNotice('')
    try {
      if (after && before && after >= before) throw new Error('After must be earlier than before.')
      const result = await invokeBridge<SearchResult[]>('search_messages', {
        serverId: server,
        query: {
          q,
          channel: channel || null,
          from: author || null,
          has: has || null,
          before: before ? Date.parse(`${before}T00:00:00Z`) : null,
          after: after ? Date.parse(`${after}T00:00:00Z`) : null,
        } satisfies SearchQuery,
      })
      if (revision !== generation.current) return
      setRows(result)
      setNotice(
        result.length === 50
          ? 'Top 50 matches. Narrow your filters to find more.'
          : `${result.length} ${result.length === 1 ? 'match' : 'matches'}`,
      )
    } catch (e) {
      if (revision === generation.current) setError(errorText(e))
    } finally {
      if (revision === generation.current) setBusy(false)
    }
  }
  return (
    <dialog className="search-dialog" ref={dialog} onCancel={close} aria-labelledby="search-title">
      <header>
        <div>
          <p className="eyebrow">This server · visible conversations</p>
          <h2 id="search-title">Search messages</h2>
        </div>
        <button className="button secondary" onClick={close}>
          Close
        </button>
      </header>
      <form
        onSubmit={(e) => {
          e.preventDefault()
          void search()
        }}
      >
        <label className="search-words">
          Search words
          <input
            autoFocus
            required
            maxLength={256}
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="Find a conversation…"
          />
        </label>
        <div className="search-filters">
          <label>
            Channel
            <select value={channel} onChange={(e) => setChannel(e.target.value)}>
              <option value="">All conversations</option>
              {[...channels].map(([id, name]) => (
                <option key={id} value={id}>
                  {name}
                </option>
              ))}
            </select>
          </label>
          <label>
            From
            <select value={author} onChange={(e) => setAuthor(e.target.value)}>
              <option value="">Anyone</option>
              {view.members.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.displayName || m.username}
                </option>
              ))}
            </select>
          </label>
          <label>
            After (UTC)
            <input type="date" value={after} onChange={(e) => setAfter(e.target.value)} />
          </label>
          <label>
            Before (UTC)
            <input type="date" value={before} onChange={(e) => setBefore(e.target.value)} />
          </label>
          <label>
            Contains
            <select value={has} onChange={(e) => setHas(e.target.value)}>
              <option value="">Any content</option>
              <option value="link">Link</option>
              <option value="file">File</option>
              <option value="image">Image</option>
            </select>
          </label>
          <button className="button primary" disabled={busy || !q.trim()}>
            {busy ? 'Searching…' : 'Search'}
          </button>
        </div>
      </form>
      {error && <p role="alert">{error}</p>}
      <p role="status">{notice}</p>
      <ol className="search-results">
        {rows
          .filter((r) => channels.has(r.channelId))
          .map((row) => (
            <li key={row.id}>
              <button
                onClick={() => {
                  try {
                    navigateMessage(messageLink(server, row.channelId, row.id))
                    close()
                  } catch (e) {
                    setError(errorText(e))
                  }
                }}
                aria-label={`Jump to message by ${row.authorName}`}
              >
                <span className="search-meta">
                  <strong>{row.authorName}</strong>
                  <span>{channels.get(row.channelId)}</span>
                  <time>{new Date(Number(row.createdAt)).toLocaleString()}</time>
                </span>
                <span className="search-excerpt">
                  {row.excerpt
                    // FTS returns these two text delimiters; no HTML is interpreted.
                    // eslint-disable-next-line no-control-regex
                    .split(/(\u0001[^\u0002]*\u0002)/g)
                    .map((part, i) =>
                      part.startsWith('\u0001') ? <mark key={i}>{part.slice(1, -1)}</mark> : part,
                    )}
                </span>
                <span className="search-jump">Jump to message →</span>
              </button>
            </li>
          ))}
      </ol>
    </dialog>
  )
}
