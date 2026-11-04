import { useEffect, useRef, useState } from 'react'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import { sendMessage } from '../../lib/api'
import { chatAction, errorText } from './api'

export function Composer({
  server,
  channel,
  disabled,
  target,
  cancel,
  sent,
  initialDraft = '',
  saveDraft,
  onBusy,
}: {
  server: string
  channel: string
  disabled: boolean
  target: { kind: 'reply' | 'edit'; message: BridgeMessage } | null
  cancel: () => void
  sent: () => void
  initialDraft?: string
  saveDraft?: (draft: string) => void
  onBusy?: (busy: boolean) => void
}) {
  const [draft, setDraft] = useState(
    target?.kind === 'edit' ? target.message.content : initialDraft,
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const input = useRef<HTMLTextAreaElement>(null)
  useEffect(() => {
    input.current?.focus()
  }, [])
  // Keep the same nonce when retrying an unchanged draft after an uncertain ack.
  const attempt = useRef<{ content: string; nonce: string } | null>(null)
  const submit = async () => {
    if (busy || disabled || !draft.trim()) return
    setBusy(true)
    onBusy?.(true)
    setError('')
    try {
      if (target?.kind === 'edit') {
        await chatAction(server, { kind: 'edit', message: target.message.id, content: draft })
      } else {
        const identity = JSON.stringify([draft, target?.message.id ?? null])
        if (attempt.current?.content !== identity)
          attempt.current = { content: identity, nonce: crypto.randomUUID() }
        await sendMessage({
          serverId: server,
          channelId: channel,
          content: draft,
          replyTo: target?.message.id ?? null,
          attachmentIds: [],
          nonce: attempt.current.nonce,
        })
      }
      setDraft('')
      if (target?.kind !== 'edit') saveDraft?.('')
      attempt.current = null
      cancel()
      sent()
      input.current?.focus()
    } catch (e) {
      setError(errorText(e))
    } finally {
      setBusy(false)
      onBusy?.(false)
    }
  }
  return (
    <form
      className="chat-composer"
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      {target && (
        <div className="chat-target">
          <span>
            {target.kind === 'edit'
              ? 'Editing message'
              : `Replying to ${target.message.authorName ?? 'Unknown author'}`}
          </span>
          <button type="button" className="button secondary" disabled={busy} onClick={cancel}>
            Cancel
          </button>
        </div>
      )}
      <label className="sr-only" htmlFor={`compose-${channel}`}>
        Message
      </label>
      <textarea
        id={`compose-${channel}`}
        ref={input}
        value={draft}
        rows={3}
        maxLength={4000}
        disabled={disabled || busy}
        placeholder={disabled ? 'You cannot send messages right now' : 'Write a message…'}
        onChange={(e) => {
          setDraft(e.target.value)
          if (target?.kind !== 'edit') saveDraft?.(e.target.value)
        }}
        onKeyDown={(e) => {
          if (e.key === 'Enter' && !e.shiftKey && !e.nativeEvent.isComposing) {
            e.preventDefault()
            void submit()
          }
          if (e.key === 'Escape' && target && !busy) cancel()
        }}
      />
      <div className="chat-compose-footer">
        <small>Enter to send · Shift + Enter for a new line</small>
        <button
          type="submit"
          className="button primary"
          disabled={busy || disabled || !draft.trim()}
        >
          {busy ? 'Sending…' : target?.kind === 'edit' ? 'Save edit' : 'Send'}
        </button>
      </div>
      {error && <p role="alert">{error} Your message is still here; retry when ready.</p>}
    </form>
  )
}
