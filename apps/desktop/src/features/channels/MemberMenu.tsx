import { openDm } from '../dm/api'
import { errorText } from '../chat/api'
import { useEffect, useRef, useState } from 'react'
import type { MemberView } from '../../lib/generated/MemberView'
import type { ServerView } from '../../lib/generated/ServerView'
import { memberAction, voiceControl } from '../voice/api'
import { runVoice, useVoiceStore } from '../voice/store'

export function MemberMenu({
  server,
  member,
  view,
  close,
  connected,
}: {
  server: string
  member: MemberView
  view: ServerView
  close: () => void
  connected: boolean
}) {
  const modal = useRef<HTMLDialogElement>(null)
  const pending = useVoiceStore((s) => s.busy)
  const [dmBusy, setDmBusy] = useState(false)
  const [dmError, setDmError] = useState('')
  const busy = pending || dmBusy || !connected
  const active = useVoiceStore((s) => s.active)
  const [volume, setVolume] = useState(member.volume)
  const [destination, setDestination] = useState('')
  const [confirm, setConfirm] = useState<'kick' | 'ban' | null>(null)
  const [reason, setReason] = useState('')
  const [copied, setCopied] = useState(false)
  const own = member.id === view.ownUserId
  const inVoice = view.participants.some((p) => p.userId === member.id)
  const serverMuted = view.participants.some((p) => p.userId === member.id && p.serverMuted)
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null
    const dialog = modal.current
    dialog?.showModal()
    return () => {
      dialog?.close()
      previous?.focus()
    }
  }, [])
  function act(
    action: string,
    options: { enabled?: boolean; volume?: number; channelId?: string; reason?: string } = {},
  ) {
    void runVoice(() =>
      memberAction({
        serverId: server,
        userId: member.id,
        action,
        enabled: options.enabled ?? null,
        volume: options.volume ?? null,
        channelId: options.channelId ?? null,
        reason: options.reason ?? null,
      }),
    )
  }
  return (
    <dialog
      ref={modal}
      className="connect-dialog onboarding-dialog member-menu"
      onKeyDown={(event) => {
        if (event.key !== 'Tab') return
        const controls = Array.from(
          event.currentTarget.querySelectorAll<HTMLElement>('button, input, select, textarea'),
        ).filter((element) => !element.matches(':disabled') && element.getClientRects().length > 0)
        if (!controls.length) return
        event.preventDefault()
        const index = controls.indexOf(document.activeElement as HTMLElement)
        const next =
          index < 0
            ? event.shiftKey
              ? controls.length - 1
              : 0
            : (index + (event.shiftKey ? -1 : 1) + controls.length) % controls.length
        controls[next]?.focus()
      }}
      onCancel={(e) => {
        if (dmBusy) e.preventDefault()
        else close()
      }}
      aria-labelledby="member-menu-title"
    >
      <header>
        <div className="live-avatar">{member.username.slice(0, 2)}</div>
        <div>
          <h2 id="member-menu-title">{member.displayName || member.username}</h2>
          <p>@{member.username}</p>
        </div>
        <button aria-label="Close member actions" className="icon-button" onClick={close}>
          ×
        </button>
      </header>
      <div className="member-menu-body">
        <p>
          {member.roleName} · {member.presence}
        </p>
        <button
          className="button secondary"
          onClick={() => {
            void navigator.clipboard
              .writeText(member.id)
              .then(() => setCopied(true))
              .catch(() => useVoiceStore.setState({ error: 'Could not copy the user ID.' }))
          }}
        >
          {copied ? 'User ID copied' : 'Copy user ID'}
        </button>
        {!own && (
          <button
            className="button primary"
            disabled={busy}
            onClick={() => {
              setDmBusy(true)
              setDmError('')
              void openDm(server, member.id)
                .then(close)
                .catch((e) => setDmError(errorText(e)))
                .finally(() => setDmBusy(false))
            }}
          >
            {dmBusy ? 'Opening…' : 'Message'}
          </button>
        )}
        {dmError && <p role="alert">{dmError}</p>}
        {own ? (
          <div className="field-row">
            <button
              className="button secondary"
              disabled={busy || active?.serverId !== server}
              onClick={() => void runVoice(() => voiceControl('mute', !active?.muted))}
            >
              {active?.muted ? 'Unmute yourself' : 'Mute yourself'}
            </button>
            <button
              className="button secondary"
              disabled={busy || active?.serverId !== server}
              onClick={() => void runVoice(() => voiceControl('deafen', !active?.deafened))}
            >
              {active?.deafened ? 'Undeafen yourself' : 'Deafen yourself'}
            </button>
          </div>
        ) : (
          <>
            <label>
              Playback volume: {volume}%
              <input
                aria-label="Playback volume"
                type="range"
                min="0"
                max="200"
                step="5"
                value={volume}
                disabled={busy || active?.serverId !== server}
                onChange={(e) => setVolume(Number(e.target.value))}
              />
            </label>
            <button
              className="button secondary"
              disabled={busy || active?.serverId !== server}
              onClick={() => act('volume', { volume })}
            >
              Apply volume
            </button>
            <button
              className="button secondary"
              disabled={busy || active?.serverId !== server}
              onClick={() => act('localMute', { enabled: !member.localMuted })}
            >
              {member.localMuted ? 'Unmute for me' : 'Mute for me'}
            </button>
            <button
              className="button secondary"
              disabled={busy || !member.canMute || !inVoice}
              title={!member.canMute ? 'Requires MUTE_MEMBERS and a higher role' : undefined}
              onClick={() => act('serverMute', { enabled: !serverMuted })}
            >
              {serverMuted ? 'Remove server mute' : 'Server mute'}
            </button>
            <label>
              Move to
              <select
                aria-label="Move to channel"
                value={destination}
                disabled={busy || !member.canMove || !inVoice}
                onChange={(e) => setDestination(e.target.value)}
              >
                <option value="">Choose voice channel</option>
                {view.channels
                  .filter((c) => c.kind === 3)
                  .map((c) => (
                    <option key={c.id} value={c.id}>
                      {c.name}
                    </option>
                  ))}
              </select>
            </label>
            <button
              className="button secondary"
              disabled={busy || !member.canMove || !inVoice || !destination}
              title={!member.canMove ? 'Requires MOVE_MEMBERS and a higher role' : undefined}
              onClick={() => act('move', { channelId: destination })}
            >
              Move member
            </button>
            <div className="field-row">
              <button
                className="button secondary"
                disabled={busy || !member.canKick}
                title="Requires KICK_MEMBERS and a higher role"
                onClick={() => setConfirm('kick')}
              >
                Kick…
              </button>
              <button
                className="button secondary"
                disabled={busy || !member.canBan}
                title="Requires BAN_MEMBERS and a higher role"
                onClick={() => setConfirm('ban')}
              >
                Ban…
              </button>
            </div>
          </>
        )}
        {confirm && (
          <section aria-label="Confirm moderation">
            <p>
              {confirm === 'ban' ? 'Permanently ban' : 'Disconnect'} @{member.username} from this
              server?
            </p>
            <label>
              Reason
              <input value={reason} maxLength={512} onChange={(e) => setReason(e.target.value)} />
            </label>
            <button className="button secondary" onClick={() => setConfirm(null)}>
              Cancel
            </button>
            <button
              className="button primary danger"
              disabled={busy}
              onClick={() => {
                act(confirm, { reason })
                setConfirm(null)
              }}
            >
              Confirm {confirm}
            </button>
          </section>
        )}
      </div>
    </dialog>
  )
}
