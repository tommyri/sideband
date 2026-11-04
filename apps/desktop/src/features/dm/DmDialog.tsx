import { useEffect, useRef, useState } from 'react'
import type { ServerView } from '../../lib/generated/ServerView'
import type { DmView } from '../../lib/generated/DmView'
import type { DmAction } from '../../lib/generated/DmAction'
import type { DmResult } from '../../lib/generated/DmResult'
import { dmAction } from './api'
import { errorText } from '../chat/api'

export type DmMode = 'create' | 'add' | 'rename' | 'leave'
export function DmDialog({
  server,
  view,
  conversation,
  mode,
  connected,
  close,
  complete,
}: {
  server: string
  view: ServerView
  conversation?: DmView
  mode: DmMode
  connected: boolean
  close: () => void
  complete: (result: DmResult) => void
}) {
  const dialog = useRef<HTMLDialogElement>(null)
  const [selected, setSelected] = useState<string[]>([])
  const [group, setGroup] = useState(false)
  const [name, setName] = useState(mode === 'rename' ? (conversation?.name ?? '') : '')
  const [query, setQuery] = useState('')
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const element = dialog.current
    element?.showModal()
    return () => {
      element?.close()
      previous?.focus()
    }
  }, [])
  const maximum =
    mode === 'add' ? Math.max(0, 10 - (conversation?.memberIds.length ?? 10)) : group ? 9 : 1
  const minimum = mode === 'create' && group ? 2 : 1
  const picking = mode === 'create' || mode === 'add'
  const eligible = view.members.filter(
    (m) => m.id !== view.ownUserId && !(mode === 'add' && conversation?.memberIds.includes(m.id)),
  )
  const visible = eligible.filter((m) =>
    `${m.displayName} ${m.username}`.toLocaleLowerCase().includes(query.toLocaleLowerCase()),
  )
  const valid =
    mode === 'rename'
      ? Boolean(name.trim())
      : !picking ||
        (selected.length >= minimum &&
          selected.length <= maximum &&
          selected.every((id) => eligible.some((m) => m.id === id)))
  const title =
    mode === 'create'
      ? 'New conversation'
      : mode === 'add'
        ? 'Add members'
        : mode === 'rename'
          ? 'Rename group'
          : 'Leave group'
  const submit = async () => {
    if (busy || !connected || !valid) return
    setBusy(true)
    setError('')
    const action: DmAction =
      mode === 'create'
        ? { kind: 'create', userIds: selected, name: group ? name : '' }
        : mode === 'add'
          ? { kind: 'add', channel: conversation!.id, userIds: selected }
          : mode === 'rename'
            ? { kind: 'rename', channel: conversation!.id, name }
            : { kind: 'leave', channel: conversation!.id }
    try {
      complete(await dmAction(server, action))
    } catch (e) {
      setError(errorText(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <dialog
      ref={dialog}
      className="connect-dialog onboarding-dialog dm-dialog"
      aria-labelledby="dm-dialog-title"
      onCancel={(e) => {
        if (busy) e.preventDefault()
        else close()
      }}
    >
      <form
        onSubmit={(e) => {
          e.preventDefault()
          void submit()
        }}
      >
        <h2 id="dm-dialog-title">{title}</h2>
        {mode === 'create' && (
          <label className="dm-option">
            <input
              type="checkbox"
              checked={group}
              disabled={busy}
              onChange={(e) => {
                setGroup(e.target.checked)
                setSelected([])
              }}
            />{' '}
            Group conversation
          </label>
        )}
        {(mode === 'rename' || (mode === 'create' && group)) && (
          <label>
            Group name{mode === 'create' ? ' (optional)' : ''}
            <input
              value={name}
              maxLength={64}
              disabled={busy}
              onChange={(e) => setName(e.target.value)}
            />
          </label>
        )}
        {picking && (
          <>
            <p>
              Choose {minimum === maximum ? maximum : `${minimum}–${maximum}`}{' '}
              {mode === 'add' ? 'new ' : ''}people. {selected.length} selected.
            </p>
            <label>
              Find members
              <input
                value={query}
                disabled={busy}
                onChange={(e) => setQuery(e.target.value)}
                placeholder="Display name or username"
              />
            </label>
            <div className="dm-picker" role="group" aria-label="Conversation participants">
              {visible.map((member) => (
                <label key={member.id} className="dm-option">
                  <input
                    type="checkbox"
                    checked={selected.includes(member.id)}
                    disabled={busy || (!selected.includes(member.id) && selected.length >= maximum)}
                    onChange={(e) =>
                      setSelected((old) =>
                        e.target.checked
                          ? [...old, member.id]
                          : old.filter((id) => id !== member.id),
                      )
                    }
                  />
                  <span>
                    {member.displayName || member.username} <small>@{member.username}</small>
                  </span>
                </label>
              ))}
              {visible.length === 0 && <p>No matching members available.</p>}
            </div>
          </>
        )}
        {mode === 'leave' && (
          <p>
            You will lose access to this conversation and its history.{' '}
            {conversation?.creatorId === view.ownUserId
              ? 'You are the creator. If you leave, nobody will be able to add members, including you, to this group.'
              : 'The creator can invite you again while they remain a member.'}
          </p>
        )}
        {error && <p role="alert">{error}</p>}
        {!connected && <p role="status">Reconnect to manage conversations.</p>}
        <footer className="dm-dialog-actions">
          <button type="button" className="button secondary" disabled={busy} onClick={close}>
            Cancel
          </button>
          <button type="submit" className="button primary" disabled={busy || !connected || !valid}>
            {busy ? 'Working…' : mode === 'create' ? 'Open conversation' : title}
          </button>
        </footer>
      </form>
    </dialog>
  )
}
