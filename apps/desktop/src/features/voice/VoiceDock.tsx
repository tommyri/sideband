import { useEffect, useState } from 'react'
import { disconnect } from '../../lib/api'
import { useAppStore } from '../../store/app-store'
import { leaveVoice, voiceControl } from './api'
import { runVoice, useVoiceStore } from './store'

function ptt(enabled: boolean) {
  void voiceControl('ptt', enabled).catch(() => {
    if (enabled) useVoiceStore.setState({ error: 'Could not open the microphone gate.' })
  })
}
export function VoiceDock() {
  const active = useVoiceStore((s) => s.active)
  const busy = useVoiceStore((s) => s.busy)
  const error = useVoiceStore((s) => s.error)
  const notice = useVoiceStore((s) => s.deviceNotice)
  const stats = useVoiceStore((s) => (active ? s.stats[active.serverId] : undefined))
  const view = useVoiceStore((s) => (active ? s.views[active.serverId] : undefined))
  const servers = useAppStore((s) => s.servers)
  const [held, setHeld] = useState(false)
  const [settings, setSettings] = useState(false)
  const server = servers.find((s) => s.id === active?.serverId)
  const channel = view?.channels.find((c) => c.id === active?.channelId)
  const own = view?.members.find((m) => m.id === view.ownUserId)
  const serverMuted = view?.participants.some(
    (p) => p.userId === view.ownUserId && p.channelId === active?.channelId && p.serverMuted,
  )
  const canTalk =
    !!active &&
    !active.muted &&
    !active.deafened &&
    !serverMuted &&
    !!channel?.canSpeak &&
    server?.status.kind === 'connected' &&
    !busy
  useEffect(() => {
    const release = () => {
      ptt(false)
      setHeld(false)
    }
    const hide = () => {
      if (document.hidden) release()
    }
    window.addEventListener('blur', release)
    window.addEventListener('pointerup', release)
    window.addEventListener('pointercancel', release)
    document.addEventListener('visibilitychange', hide)
    return () => {
      ptt(false)
      window.removeEventListener('blur', release)
      window.removeEventListener('pointerup', release)
      window.removeEventListener('pointercancel', release)
      document.removeEventListener('visibilitychange', hide)
    }
  }, [active?.serverId, active?.channelId])
  useEffect(() => {
    if (!canTalk) ptt(false)
  }, [canTalk])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      if (
        !(event.ctrlKey || event.metaKey) ||
        !event.shiftKey ||
        event.repeat ||
        document.querySelector('dialog[open]')
      )
        return
      if (
        event.target instanceof HTMLElement &&
        event.target.closest('input, textarea, select, [contenteditable="true"]')
      )
        return
      const state = useVoiceStore.getState()
      if (state.busy) return
      if (state.active && ['m', 'd'].includes(event.key.toLowerCase())) {
        event.preventDefault()
        void runVoice(() =>
          voiceControl(
            event.key.toLowerCase() === 'm' ? 'mute' : 'deafen',
            event.key.toLowerCase() === 'm' ? !state.active!.muted : !state.active!.deafened,
          ),
        )
      }
      if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
        const server = useAppStore.getState().selectedServerId
        const channels = server
          ? state.views[server]?.channels.filter((c) => c.kind !== 1)
          : undefined
        if (!server || !channels?.length) return
        event.preventDefault()
        const index = channels.findIndex((c) => c.id === state.selected[server])
        const next =
          channels[
            (index + (event.key === 'ArrowDown' ? 1 : -1) + channels.length) % channels.length
          ]
        if (next) state.select(server, next.id)
      }
    }
    window.addEventListener('keydown', shortcut)
    return () => window.removeEventListener('keydown', shortcut)
  }, [])
  function gate(open: boolean) {
    ptt(open && canTalk)
    setHeld(open && canTalk)
  }
  return (
    <div className="voice-dock-container">
      {(error || notice) && (
        <div className="voice-feedback">
          <p role={error ? 'alert' : 'status'}>{error || notice}</p>
          <button
            aria-label="Dismiss voice notice"
            onClick={() => useVoiceStore.setState({ error: '', deviceNotice: '' })}
          >
            ×
          </button>
        </div>
      )}
      {active && (
        <section className="voice-dock" aria-label="Voice controls">
          <div className="call-identity">
            <span
              className={`quality-dot ${server?.status.kind === 'connected' ? (stats && stats.rttMs > 300 ? 'poor' : stats && stats.rttMs > 150 ? 'fair' : 'good') : 'poor'}`}
              aria-label={`Connection ${server?.status.kind ?? 'disconnected'}`}
            />
            <div>
              <strong>
                {own?.displayName || own?.username || 'You'}
                {own && <small> @{own.username}</small>}
              </strong>
              <p>
                {server?.label} / {channel?.name ?? 'Voice'} · {channel?.profile ?? 'voice'}
              </p>
              <small>
                {stats
                  ? `${Math.round(stats.encoderBitrate / 1000)} kbps · ${stats.rttMs} ms network RTT`
                  : 'Measuring connection…'}
              </small>
            </div>
          </div>
          <button
            className={`button ${held && canTalk ? 'primary' : 'secondary'}`}
            disabled={!canTalk}
            onPointerDown={(e) => {
              if (e.button !== 0) return
              e.currentTarget.setPointerCapture(e.pointerId)
              gate(true)
            }}
            onPointerUp={() => gate(false)}
            onPointerCancel={() => gate(false)}
            onLostPointerCapture={() => gate(false)}
            onBlur={() => gate(false)}
            onKeyDown={(e) => {
              if (e.key === ' ' || e.key === 'Enter') {
                e.preventDefault()
                if (!e.repeat) gate(true)
              }
            }}
            onKeyUp={(e) => {
              if (e.key === ' ' || e.key === 'Enter') {
                e.preventDefault()
                gate(false)
              }
            }}
            title={
              serverMuted
                ? 'A moderator muted you'
                : !channel?.canSpeak
                  ? 'Requires SPEAK permission'
                  : 'Hold mouse, Space or Enter while focused'
            }
          >
            {held && canTalk ? 'Talking…' : 'Hold to talk'}
          </button>
          <button
            className="button secondary"
            aria-pressed={active.muted}
            disabled={busy || server?.status.kind !== 'connected'}
            onClick={() => void runVoice(() => voiceControl('mute', !active.muted))}
          >
            {active.muted ? 'Unmute' : 'Mute'}
          </button>
          <button
            className="button secondary"
            aria-pressed={active.deafened}
            disabled={busy || server?.status.kind !== 'connected'}
            onClick={() => void runVoice(() => voiceControl('deafen', !active.deafened))}
          >
            {active.deafened ? 'Undeafen' : 'Deafen'}
          </button>
          <button
            className="button secondary"
            aria-label="Voice settings"
            onClick={() => setSettings(!settings)}
          >
            ⚙
          </button>
          <button
            className="button secondary"
            disabled={busy}
            onClick={() => void runVoice(leaveVoice)}
          >
            Leave
          </button>
          <button
            className="button secondary"
            disabled={busy}
            onClick={() => void runVoice(() => disconnect(active.serverId))}
          >
            Disconnect
          </button>
        </section>
      )}
      {settings && active && (
        <p className="voice-settings-note">
          Using your system-default microphone and output with in-app push-to-talk. Device
          selection, processing controls and global hotkeys will be added in a later testing build.
        </p>
      )}
    </div>
  )
}
