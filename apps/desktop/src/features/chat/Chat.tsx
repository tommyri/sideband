import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import { chatAction, errorText } from './api'
import { useChat } from './useChat'
import { atBottom, canEdit, day, grouped } from './model'
import { Composer } from './Composer'
import './chat.css'

const empty: BridgeMessage[] = []
export function Chat({
  server,
  channel,
  name,
  connected,
}: {
  server: string
  channel: string
  name: string
  connected: boolean
}) {
  const { view, error, setError, loading, hasOlder, load, refresh } = useChat(server, channel)
  const messages = view?.messages ?? empty
  const scroll = useRef<HTMLDivElement>(null)
  const initialRead = useRef<string | null | undefined>(undefined)
  const readSent = useRef<string | null>(null)
  const readBusy = useRef(false)
  const [bottom, setBottom] = useState(true)
  const [target, setTarget] = useState<{ kind: 'edit' | 'reply'; message: BridgeMessage } | null>(
    null,
  )
  const [deleting, setDeleting] = useState<string | null>(null)
  const [deleteBusy, setDeleteBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const [draft, setDraft] = useState('')
  const [sending, setSending] = useState(false)
  const getItemKey = useCallback((index: number) => messages[index]!.id, [messages])
  // This app does not enable React Compiler; keep the mutable virtualizer local.
  // eslint-disable-next-line react-hooks/incompatible-library
  const virtual = useVirtualizer({
    count: messages.length,
    getScrollElement: () => scroll.current,
    getItemKey,
    estimateSize: () => 120,
    overscan: 6,
    anchorTo: 'end',
    followOnAppend: true,
    scrollEndThreshold: 8,
  })
  const byId = useMemo(() => new Map(messages.map((m) => [m.id, m])), [messages])
  useLayoutEffect(() => {
    if (view && initialRead.current === undefined) {
      initialRead.current = view.lastRead
      virtual.scrollToEnd()
    }
  }, [view, virtual])
  useEffect(() => {
    const element = scroll.current
    if (!element) return
    const observer = new ResizeObserver(() => setBottom(atBottom(element)))
    observer.observe(element)
    return () => observer.disconnect()
  }, [])
  // Read only what is actually visible in the foreground at the latest edge.
  // A delayed ack must not suppress the next message arriving while it is pending.
  const markRead = useCallback(async () => {
    const last = messages.at(-1)?.id
    if (
      !connected ||
      !last ||
      !scroll.current ||
      !atBottom(scroll.current) ||
      !document.hasFocus() ||
      document.visibilityState !== 'visible' ||
      readBusy.current ||
      last === readSent.current ||
      (view?.lastRead && last <= view.lastRead)
    )
      return
    readBusy.current = true
    try {
      await chatAction(server, { kind: 'read', channel, message: last })
      readSent.current = last
    } catch (e) {
      setError(errorText(e))
    } finally {
      readBusy.current = false
    }
  }, [messages, connected, server, channel, view?.lastRead, setError])
  useEffect(() => {
    const timer = window.setInterval(() => {
      void markRead()
    }, 1000)
    window.addEventListener('focus', markRead)
    document.addEventListener('visibilitychange', markRead)
    return () => {
      clearInterval(timer)
      window.removeEventListener('focus', markRead)
      document.removeEventListener('visibilitychange', markRead)
    }
  }, [markRead])
  const jump = (id: string) => {
    const index = messages.findIndex((m) => m.id === id)
    if (index < 0) {
      setNotice('That message is not loaded. Load older messages to find it.')
      return
    }
    virtual.scrollToIndex(index, { align: 'center' })
    setBottom(false)
    setNotice(`Jumped to message by ${messages[index]!.authorName ?? 'Unknown author'}.`)
  }
  const remove = async () => {
    if (!deleting || deleteBusy) return
    setDeleteBusy(true)
    try {
      await chatAction(server, { kind: 'delete', message: deleting })
      setDeleting(null)
      await refresh()
    } catch (e) {
      setError(errorText(e))
    } finally {
      setDeleteBusy(false)
    }
  }
  const firstUnread = messages.findIndex(
    (m) =>
      initialRead.current !== undefined &&
      (initialRead.current === null || m.id > initialRead.current),
  )
  return (
    <section className="chat" aria-label={`Messages in ${name}`}>
      <div className="chat-history-tools">
        <button
          className="button secondary"
          disabled={loading || !connected || !hasOlder}
          onClick={() => void load(messages[0]?.id ?? null)}
        >
          {loading
            ? 'Loading messages…'
            : hasOlder
              ? 'Load older messages'
              : 'Beginning of conversation'}
        </button>
        {!bottom && (
          <button
            className="button secondary"
            onClick={() => {
              virtual.scrollToEnd()
              setBottom(true)
            }}
          >
            Jump to latest
          </button>
        )}
      </div>
      {error && (
        <div role="alert">
          {error}{' '}
          <button
            className="button secondary"
            onClick={() => void load(null)}
            disabled={loading || !connected}
          >
            Retry
          </button>
        </div>
      )}
      {notice && (
        <p role="status" className="chat-notice">
          {notice}
        </p>
      )}
      <div
        className="chat-scroll"
        ref={scroll}
        tabIndex={0}
        aria-label="Message history"
        onScroll={() => {
          if (scroll.current) setBottom(atBottom(scroll.current))
          void markRead()
        }}
      >
        {!loading && !messages.length && (
          <p className="chat-empty">No messages yet. Start the conversation.</p>
        )}
        <div
          role="list"
          aria-label="Messages"
          style={{ height: virtual.getTotalSize(), position: 'relative' }}
        >
          {virtual.getVirtualItems().map((row) => {
            const message = messages[row.index]!
            const previous = messages[row.index - 1]
            const group = grouped(previous, message)
            return (
              <article
                role="listitem"
                aria-posinset={row.index + 1}
                aria-setsize={messages.length}
                key={row.key}
                data-index={row.index}
                ref={virtual.measureElement}
                className={`chat-message ${group ? 'grouped' : ''}`}
                tabIndex={0}
                style={{
                  position: 'absolute',
                  top: 0,
                  left: 0,
                  width: '100%',
                  transform: `translateY(${row.start}px)`,
                }}
              >
                {(!previous || day(previous.createdAt) !== day(message.createdAt)) && (
                  <div className="chat-day">{day(message.createdAt)}</div>
                )}
                {row.index === firstUnread && <div className="chat-unread">New messages</div>}
                {!group && (
                  <header>
                    <strong>{message.authorName ?? 'Unknown author'}</strong>
                    {message.username && <span>@{message.username}</span>}
                    {!message.authorId && <span>Integration</span>}
                    <time dateTime={new Date(message.createdAt).toISOString()}>
                      {new Date(message.createdAt).toLocaleTimeString([], {
                        hour: '2-digit',
                        minute: '2-digit',
                      })}
                    </time>
                  </header>
                )}
                {message.replyTo && (
                  <button className="chat-reply" onClick={() => jump(message.replyTo!)}>
                    ↳{' '}
                    {byId.get(message.replyTo)?.content.slice(0, 100) ??
                      'Reply to an earlier message'}
                  </button>
                )}
                <p className="chat-body">
                  {message.content}
                  {message.editedAt !== null && <small> (edited)</small>}
                </p>
                {message.attachmentCount > 0 && (
                  <small>
                    {message.attachmentCount} attachment(s) · Viewing attachments is coming in a
                    later build.
                  </small>
                )}
                <div className="chat-actions">
                  <button
                    disabled={sending || !connected || !view?.canSend}
                    onClick={() => setTarget({ kind: 'reply', message })}
                  >
                    Reply
                  </button>
                  {canEdit(message, view?.ownUserId ?? null) && (
                    <button
                      disabled={sending || !connected}
                      onClick={() => setTarget({ kind: 'edit', message })}
                    >
                      Edit
                    </button>
                  )}
                  {((message.authorId !== null && message.authorId === view?.ownUserId) ||
                    view?.canManage) && (
                    <button disabled={!connected} onClick={() => setDeleting(message.id)}>
                      Delete
                    </button>
                  )}
                  <button
                    onClick={() =>
                      void navigator.clipboard.writeText(message.content).then(
                        () => setNotice('Message copied.'),
                        (e) => setError(errorText(e)),
                      )
                    }
                  >
                    Copy
                  </button>
                </div>
              </article>
            )
          })}
        </div>
      </div>
      {deleting && (
        <div className="chat-delete" role="group" aria-label="Confirm message deletion">
          <p>Delete this message? This cannot be undone.</p>
          <button
            className="button primary"
            disabled={deleteBusy || !connected}
            onClick={() => void remove()}
          >
            Delete message
          </button>
          <button
            className="button secondary"
            disabled={deleteBusy}
            onClick={() => setDeleting(null)}
          >
            Cancel
          </button>
        </div>
      )}
      <Composer
        key={target?.kind === 'edit' ? target.message.id : 'new'}
        initialDraft={draft}
        saveDraft={setDraft}
        onBusy={setSending}
        server={server}
        channel={channel}
        disabled={sending || !connected || !view?.canSend}
        target={target}
        cancel={() => setTarget(null)}
        sent={() => {
          void refresh()
          virtual.scrollToEnd()
          setBottom(true)
        }}
      />
    </section>
  )
}
