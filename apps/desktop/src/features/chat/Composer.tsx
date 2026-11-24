import { CaseSensitive, Paperclip, Reply, SendHorizontal, X } from 'lucide-react'
import { ActionMenu } from '../../components/ActionMenu'
import { useUploads } from './useUploads'
import { fileSize } from './attachments'
import { draftNonce, clearDraftNonce } from './navigation'
import { completeOptions, completionAt, wrapSelection, type ChatMember } from './rich-model'
import { useEffect, useLayoutEffect, useRef, useState } from 'react'
import type { BridgeMessage } from '../../lib/generated/BridgeMessage'
import { sendMessage } from '../../lib/api'
import { chatAction, errorText, notifyTyping } from './api'

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
  members = [],
  editLast,
  draftIdentity = JSON.stringify([server, channel]),
  canAttach = false,
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
  members?: ChatMember[]
  editLast?: () => void
  canAttach?: boolean
  draftIdentity?: string
}) {
  const uploads = useUploads(draftIdentity, server, channel)
  const fileInput = useRef<HTMLInputElement>(null)
  const [draft, setDraft] = useState(
    target?.kind === 'edit' ? target.message.content : initialDraft,
  )
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const input = useRef<HTMLTextAreaElement>(null)
  // Older webviews need an explicit height when the text wraps or the pane resizes.
  useLayoutEffect(() => {
    const field = input.current
    if (!field || globalThis.CSS?.supports('field-sizing', 'content')) return
    const fit = () => {
      field.style.height = 'auto'
      field.style.height = `${field.scrollHeight + field.offsetHeight - field.clientHeight}px`
    }
    fit()
    if (typeof ResizeObserver === 'undefined') return
    let width = field.clientWidth
    const observer = new ResizeObserver(() => {
      if (width !== field.clientWidth) {
        width = field.clientWidth
        fit()
      }
    })
    observer.observe(field)
    return () => observer.disconnect()
  }, [draft])
  const [cursor, setCursor] = useState(initialDraft.length)
  const [activeOption, setActiveOption] = useState(0)
  const [dismissed, setDismissed] = useState(false)
  const completion = completionAt(draft, cursor)
  const options =
    !dismissed && !disabled && !busy && completion
      ? completeOptions(completion.kind, completion.query, members)
      : []
  const change = (value: string, position: number) => {
    setDraft(value)
    setCursor(position)
    setDismissed(false)
    setActiveOption(0)
    if (target?.kind !== 'edit') saveDraft?.(value)
    if (value.trim() && !disabled && target?.kind !== 'edit') notifyTyping(server, channel)
  }
  const insertTrigger = (trigger: string) => {
    const start = input.current?.selectionStart ?? cursor
    const end = input.current?.selectionEnd ?? cursor
    const token = (start > 0 && !/\s/.test(draft[start - 1]!) ? ' ' : '') + trigger
    if (draft.length + token.length > 4000) return
    change(draft.slice(0, start) + token + draft.slice(end), start + token.length)
    requestAnimationFrame(() => {
      input.current?.focus()
      input.current?.setSelectionRange(start + token.length, start + token.length)
    })
  }

  const selectOption = (index: number) => {
    const option = options[index]
    if (!option || !completion) return
    const value = draft.slice(0, completion.start) + option.value + draft.slice(completion.end)
    if (value.length > 4000) return
    const position = completion.start + option.value.length
    change(value, position)
    setDismissed(true)
    requestAnimationFrame(() => {
      input.current?.focus()
      input.current?.setSelectionRange(position, position)
    })
  }
  const format = (marker: string) => {
    const element = input.current
    if (!element) return
    const next = wrapSelection(draft, element.selectionStart, element.selectionEnd, marker)
    if (next.value.length > 4000) return
    change(next.value, next.end)
    requestAnimationFrame(() => {
      element.focus()
      element.setSelectionRange(next.start, next.end)
    })
  }
  useEffect(() => {
    input.current?.focus()
  }, [])
  // Keep the same nonce when retrying an unchanged draft after an uncertain ack.
  const submit = async () => {
    if (
      busy ||
      disabled ||
      (!draft.trim() && !uploads.items.length) ||
      (target?.kind !== 'edit' && (!uploads.ready || (uploads.items.length > 0 && !canAttach)))
    )
      return
    setBusy(true)
    onBusy?.(true)
    setError('')
    try {
      if (target?.kind === 'edit') {
        await chatAction(server, { kind: 'edit', message: target.message.id, content: draft })
      } else {
        const identity = JSON.stringify([
          draft,
          target?.message.id ?? null,
          uploads.items.map((i) => i.id),
        ])
        await sendMessage({
          serverId: server,
          channelId: channel,
          content: draft,
          replyTo: target?.message.id ?? null,
          attachmentIds: uploads.items.map((i) => i.id!),
          nonce: draftNonce(draftIdentity, identity),
        })
      }
      setDraft('')
      setCursor(0)
      setDismissed(true)
      if (target?.kind !== 'edit') {
        saveDraft?.('')
        uploads.clear()
      }
      clearDraftNonce(draftIdentity)
      cancel()
      sent()
    } catch (e) {
      setError(errorText(e))
    } finally {
      setBusy(false)
      onBusy?.(false)
    }
  }
  const addFiles = (files: File[]) => {
    if (!canAttach || disabled || busy || target?.kind === 'edit') return
    try {
      uploads.add(files)
      setError('')
    } catch (e) {
      setError(errorText(e))
    }
  }
  return (
    <form
      className="chat-composer"
      onDragOver={(e) => {
        if (e.dataTransfer.types.includes('Files')) e.preventDefault()
      }}
      onDrop={(e) => {
        if (e.dataTransfer.files.length) {
          e.preventDefault()
          addFiles(Array.from(e.dataTransfer.files))
        }
      }}
      onPaste={(e) => {
        if (e.clipboardData.files.length) {
          e.preventDefault()
          addFiles(Array.from(e.clipboardData.files))
        }
      }}
      onSubmit={(e) => {
        e.preventDefault()
        void submit()
      }}
    >
      {target && (
        <div className="chat-target">
          {target.kind === 'reply' && <Reply size={16} aria-hidden="true" />}
          <span className="target-label">
            {target.kind === 'edit'
              ? 'Editing message'
              : `Replying to ${target.message.authorName ?? 'Unknown author'}`}
          </span>
          {target.kind === 'reply' && (
            <span className="target-excerpt">
              {target.message.content.trim() || target.message.attachments[0]?.filename}
            </span>
          )}
          <button
            type="button"
            className="icon-button"
            aria-label={target.kind === 'reply' ? 'Cancel reply' : 'Cancel edit'}
            disabled={busy}
            onClick={cancel}
          >
            <X size={17} />
          </button>
        </div>
      )}
      {target?.kind !== 'edit' && uploads.items.length > 0 && (
        <ul className="chat-upload-list" aria-label="Attachments to send">
          {uploads.items.map((item) => (
            <li key={item.key}>
              <span>
                {item.file.name} · {fileSize(item.file.size)} ·{' '}
                {item.status === 'ready'
                  ? 'Ready'
                  : item.status === 'reading'
                    ? 'Preparing…'
                    : item.status === 'uploading'
                      ? 'Uploading…'
                      : item.error}
              </span>
              {(item.status === 'reading' || item.status === 'uploading') && (
                <progress aria-label={`Uploading ${item.file.name}`} />
              )}
              {item.status === 'error' && (
                <button
                  type="button"
                  disabled={busy || disabled || !canAttach}
                  onClick={() => uploads.retry(item.key)}
                >
                  Retry upload
                </button>
              )}
              <button type="button" disabled={busy} onClick={() => uploads.remove(item.key)}>
                {item.status === 'uploading' || item.status === 'reading'
                  ? 'Cancel upload'
                  : 'Remove attachment'}
              </button>
            </li>
          ))}
        </ul>
      )}
      <input
        type="file"
        multiple
        ref={fileInput}
        hidden
        aria-label="Attach files"
        onChange={(e) => {
          addFiles(Array.from(e.target.files ?? []))
          e.target.value = ''
        }}
      />
      <div className="chat-format-tools">
        <button
          type="button"
          className="compose-tool"
          aria-label="Attach"
          disabled={disabled || busy || !canAttach || target?.kind === 'edit'}
          title="Attach files (up to 25 MiB each)"
          onClick={() => fileInput.current?.click()}
        >
          <Paperclip size={18} />
        </button>
        <ActionMenu
          className="compose-tool"
          label="Message formatting"
          disabled={disabled || busy}
          actions={[
            { label: 'Bold', onSelect: () => format('**') },
            { label: 'Italic', onSelect: () => format('*') },
            { label: 'Code', onSelect: () => format('`') },
            { label: 'Mention', onSelect: () => insertTrigger('@') },
            { label: 'Emoji', onSelect: () => insertTrigger(':') },
          ]}
        >
          <CaseSensitive size={20} />
        </ActionMenu>
      </div>
      {options.length > 0 && (
        <div
          role="listbox"
          id={`completion-${channel}`}
          aria-label="Message completions"
          className="chat-completions"
        >
          {options.map((option, index) => (
            <button
              type="button"
              role="option"
              aria-selected={index === activeOption}
              id={`completion-${channel}-${index}`}
              tabIndex={-1}
              key={option.value}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => selectOption(index)}
            >
              {option.label}
            </button>
          ))}
        </div>
      )}
      <label className="sr-only" htmlFor={`compose-${channel}`}>
        Message
      </label>
      <textarea
        id={`compose-${channel}`}
        ref={input}
        value={draft}
        aria-autocomplete="list"
        aria-controls={options.length ? `completion-${channel}` : undefined}
        aria-activedescendant={options.length ? `completion-${channel}-${activeOption}` : undefined}
        onSelect={(e) => setCursor(e.currentTarget.selectionStart)}
        rows={1}
        aria-describedby={`compose-hint-${channel}`}
        maxLength={4000}
        disabled={disabled || busy}
        placeholder={disabled ? 'You cannot send messages right now' : 'Write a message…'}
        onChange={(e) => change(e.target.value, e.target.selectionStart)}
        onKeyDown={(e) => {
          if (e.nativeEvent.isComposing) return
          if ((e.ctrlKey || e.metaKey) && ['b', 'i', 'e'].includes(e.key.toLowerCase())) {
            e.preventDefault()
            format(e.key.toLowerCase() === 'b' ? '**' : e.key.toLowerCase() === 'i' ? '*' : '`')
            return
          }
          if (options.length) {
            if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
              e.preventDefault()
              setActiveOption(
                (old) => (old + (e.key === 'ArrowDown' ? 1 : options.length - 1)) % options.length,
              )
              return
            }
            if ((e.key === 'Enter' || e.key === 'Tab') && !e.shiftKey) {
              e.preventDefault()
              selectOption(activeOption)
              return
            }
            if (e.key === 'Escape') {
              e.preventDefault()
              setDismissed(true)
              return
            }
          }
          if (e.key === 'ArrowUp' && !draft && !target && editLast) {
            e.preventDefault()
            editLast()
            return
          }
          if (e.key === 'Enter' && !e.shiftKey) {
            e.preventDefault()
            void submit()
          }
          if (e.key === 'Escape' && target && !busy) cancel()
        }}
      />
      <span className="sr-only" id={`compose-hint-${channel}`}>
        Enter to send · Shift + Enter for a new line
      </span>
      <button
        type="submit"
        className="button primary chat-send"
        disabled={
          busy ||
          disabled ||
          (!draft.trim() && !uploads.items.length) ||
          (target?.kind !== 'edit' && (!uploads.ready || (uploads.items.length > 0 && !canAttach)))
        }
      >
        <SendHorizontal size={16} aria-hidden="true" />{' '}
        {busy ? 'Sending…' : target?.kind === 'edit' ? 'Save edit' : 'Send'}
      </button>
      {error && <p role="alert">{error} Your message is still here; retry when ready.</p>}
    </form>
  )
}
