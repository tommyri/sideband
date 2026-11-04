import { MessageFiles } from './MessageFiles'
import { RichMessage } from './RichMessage'
import { canonicalId, messageLink } from './rich-model'
import { draftKey, getDraft, saveDraft, useChatNavigation } from './navigation'
import { useTyping } from './useTyping'
import { useVoiceStore } from '../voice/store'
import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import { chatAction, errorText, messageContext } from './api'
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
  const serverView = useVoiceStore((s) => s.views[server])
  const own = view?.ownUserId ?? serverView?.ownUserId ?? null
  const members = useMemo(
    () => new Map((serverView?.members ?? []).map((m) => [canonicalId(m.id) ?? m.id, m])),
    [serverView?.members],
  )
  const draftId = draftKey(server, own ?? '', channel)
  const typing = useTyping(server, channel, own)
  const navigation = useChatNavigation((s) => s.request)
  const [pendingJump, setPendingJump] = useState<string | null>(null)
  const [focusedMessage, setFocusedMessage] = useState<string | null>(null)
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
    const last = [...messages].reverse().find((m) => !m.deleted)?.id
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
  const focusComposer = () =>
    scroll.current?.closest('.chat')?.querySelector<HTMLTextAreaElement>('textarea')?.focus()
  const focusMessage = useCallback(
    (id: string) => {
      const index = messages.findIndex((m) => m.id === id)
      if (index < 0) return
      setFocusedMessage(id)
      virtual.scrollToIndex(index, { align: 'auto' })
      let tries = 0
      const focus = () => {
        if (!scroll.current) return
        const row = scroll.current.querySelector<HTMLElement>(`[data-message-id="${id}"]`)
        if (row) row.focus({ preventScroll: true })
        else if (++tries < 10) requestAnimationFrame(focus)
      }
      requestAnimationFrame(focus)
    },
    [messages, virtual],
  )
  const jump = useCallback(
    async (id: string) => {
      setPendingJump(id)
      if (messages.some((m) => m.id === id)) return
      try {
        await messageContext(server, channel, id)
        await refresh()
      } catch (e) {
        setError(errorText(e))
        setPendingJump(null)
      }
    },
    [messages, server, channel, refresh, setError],
  )
  useEffect(() => {
    if (navigation?.server === server && navigation.channel === channel)
      void jump(navigation.message)
    // A navigation request is consumed once, not again for each cache refresh.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [navigation?.sequence, server, channel])
  useLayoutEffect(() => {
    if (pendingJump && byId.has(pendingJump)) {
      focusMessage(pendingJump)
      setBottom(false)
      setPendingJump(null)
      setNotice('Message loaded. Press Escape to return to the composer.')
    }
  }, [pendingJump, byId, focusMessage])
  const copy = (value: string, label: string) =>
    void navigator.clipboard.writeText(value).then(
      () => {
        setNotice(`${label} copied.`)
        focusComposer()
      },
      (e) => setError(errorText(e)),
    )
  const remove = async () => {
    if (!deleting || deleteBusy) return
    setDeleteBusy(true)
    try {
      await chatAction(server, { kind: 'delete', message: deleting })
      setDeleting(null)
      await refresh()
      focusComposer()
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
        onKeyDown={(e) => {
          const element = e.target as HTMLElement
          if (e.key === 'Escape') {
            e.preventDefault()
            focusComposer()
            return
          }
          if (element !== e.currentTarget && element.getAttribute('role') !== 'listitem') return
          const index = messages.findIndex((m) => m.id === focusedMessage)
          const page = Math.max(1, Math.floor((scroll.current?.clientHeight ?? 120) / 120))
          const next =
            e.key === 'Home'
              ? 0
              : e.key === 'End'
                ? messages.length - 1
                : e.key === 'ArrowDown'
                  ? index + 1
                  : e.key === 'ArrowUp'
                    ? index < 0
                      ? messages.length - 1
                      : index - 1
                    : e.key === 'PageDown'
                      ? index + page
                      : e.key === 'PageUp'
                        ? index - page
                        : null
          if (next !== null && messages.length) {
            e.preventDefault()
            focusMessage(messages[Math.max(0, Math.min(messages.length - 1, next))]!.id)
          }
          if (e.key === 'Enter' && element !== e.currentTarget) {
            e.preventDefault()
            element.querySelector<HTMLButtonElement>('button')?.focus()
          }
        }}
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
                data-message-id={message.id}
                onFocus={() => setFocusedMessage(message.id)}
                ref={virtual.measureElement}
                className={`chat-message ${group ? 'grouped' : ''}`}
                tabIndex={focusedMessage === message.id ? 0 : -1}
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
                  <button className="chat-reply" onClick={() => void jump(message.replyTo!)}>
                    ↳{' '}
                    {byId.get(message.replyTo)?.content.slice(0, 100) ||
                      byId.get(message.replyTo)?.attachments[0]?.filename ||
                      'Reply to an earlier message'}
                  </button>
                )}
                {message.deleted ? (
                  <p className="chat-deleted">Message deleted</p>
                ) : (
                  <div className="chat-body">
                    <RichMessage content={message.content} members={members} own={own} />
                    {message.editedAt !== null && <small> (edited)</small>}
                  </div>
                )}
                {!message.deleted && (
                  <MessageFiles
                    server={server}
                    message={message.id}
                    attachments={message.attachments}
                    previews={message.previews}
                  />
                )}
                {!message.deleted && (
                  <div className="chat-actions">
                    <button
                      disabled={sending || !connected || !view?.canSend}
                      onClick={() => {
                        setTarget({ kind: 'reply', message })
                        focusComposer()
                      }}
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
                    <button onClick={() => copy(message.content, 'Message')}>Copy</button>
                    <button onClick={() => copy(message.id, 'Message ID')}>Copy ID</button>
                    <button
                      onClick={() => copy(messageLink(server, channel, message.id), 'Message link')}
                    >
                      Copy link
                    </button>
                  </div>
                )}
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
            onClick={() => {
              setDeleting(null)
              focusComposer()
            }}
          >
            Cancel
          </button>
        </div>
      )}
      {typing.length > 0 && connected && (
        <p role="status" className="chat-typing">
          {typing
            .slice(0, 3)
            .map((id) => members.get(id)?.displayName || members.get(id)?.username || 'Someone')
            .join(', ')}{' '}
          {typing.length === 1 ? 'is' : 'are'} typing…
        </p>
      )}
      <Composer
        key={`${draftId}:${target?.kind === 'edit' ? target.message.id : 'new'}`}
        canAttach={view?.canAttach ?? false}
        initialDraft={getDraft(draftId)}
        draftIdentity={draftId}
        saveDraft={(value) => saveDraft(draftId, value)}
        members={serverView?.members ?? []}
        editLast={() => {
          const last = [...messages].reverse().find((m) => !m.deleted && canEdit(m, own))
          if (last) setTarget({ kind: 'edit', message: last })
        }}
        onBusy={setSending}
        server={server}
        channel={channel}
        disabled={sending || !connected || !view?.canSend}
        target={target}
        cancel={() => {
          setTarget(null)
          requestAnimationFrame(focusComposer)
        }}
        sent={() => {
          void refresh()
          virtual.scrollToEnd()
          setBottom(true)
        }}
      />
    </section>
  )
}
