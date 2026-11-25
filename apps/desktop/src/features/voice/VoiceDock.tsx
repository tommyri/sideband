import {
  ChevronDown,
  Headphones,
  HeadphoneOff,
  Mic,
  MicOff,
  PhoneOff,
  SlidersHorizontal,
} from 'lucide-react'
import { StreamControls } from '../stream/StreamControls'
import { useShortcuts } from '../settings/shortcuts/store'
import { useEffect, useState } from 'react'
import { AudioSettings } from '../settings/audio/AudioSettings'
import { environment, useAudioStore } from '../settings/audio/store'
import { isTauri } from '../../lib/tauri'
import { useAppStore } from '../../store/app-store'
import { joinVoice, leaveVoice, voiceControl } from './api'
import { runVoice, useVoiceStore } from './store'
import { useMicrophoneGuidance } from './microphone'
import { voiceShortcut } from './shortcuts'

function ptt(enabled: boolean) {
  void voiceControl('ptt', enabled).catch(() => {
    if (enabled) useVoiceStore.setState({ error: 'Could not open the microphone gate.' })
  })
}
export function VoiceDock() {
  const globalPtt = useShortcuts(
    (s) => !!(s.snapshot?.preferences.ptt || s.snapshot?.preferences.portal),
  )
  const preferences = useAudioStore((s) => s.preferences)
  useEffect(() => {
    if (isTauri()) void environment().catch(() => {})
  }, [])
  const active = useVoiceStore((s) => s.active)
  const recovery = useVoiceStore((s) => s.recovery)
  const joining = useVoiceStore((s) => s.joining)
  const busy = useVoiceStore((s) => s.busy)
  const error = useVoiceStore((s) => s.error)
  const notice = useVoiceStore((s) => s.deviceNotice)
  const route = useVoiceStore((s) => s.deviceRoute)
  const stats = useVoiceStore((s) => (active ? s.stats[active.serverId] : undefined))
  const call = recovery ?? active
  const view = useVoiceStore((s) => (call ? s.views[call.serverId] : undefined))
  const servers = useAppStore((s) => s.servers)
  const [settings, setSettings] = useState(false)
  const server = servers.find((s) => s.id === call?.serverId)
  const channel = view?.channels.find((c) => c.id === call?.channelId)
  const own = view?.members.find((m) => m.id === view.ownUserId)
  const mic = useMicrophoneGuidance({
    server: active?.serverId,
    view,
    channel,
    connection: server?.status.kind,
  })
  const canTalk = mic.canPress && !busy && !recovery
  useEffect(() => {
    const release = () => {
      ptt(false)
    }
    const hide = () => {
      if (document.hidden) release()
    }
    window.addEventListener('blur', release)
    document.addEventListener('visibilitychange', hide)
    return () => {
      ptt(false)
      window.removeEventListener('blur', release)
      document.removeEventListener('visibilitychange', hide)
    }
  }, [active?.serverId, active?.channelId])
  useEffect(() => {
    if (!canTalk) ptt(false)
  }, [canTalk])
  useEffect(() => {
    const shortcut = (event: KeyboardEvent) => {
      const action = voiceShortcut(event)
      if (!action || event.repeat || document.querySelector('dialog[open]')) return
      if (
        event.target instanceof HTMLElement &&
        event.target.closest('input, textarea, select, [contenteditable="true"]')
      )
        return
      const state = useVoiceStore.getState()
      if (state.busy) return
      if (state.active && (action === 'mute' || action === 'deafen')) {
        event.preventDefault()
        const enabled = action === 'mute' ? !state.active.muted : !state.active.deafened
        void runVoice(() => voiceControl(action, enabled))
      }
      if (action === 'previous' || action === 'next') {
        const server = useAppStore.getState().selectedServerId
        const channels = server
          ? state.views[server]?.channels.filter((c) => c.kind !== 1)
          : undefined
        if (!server || !channels?.length) return
        event.preventDefault()
        const index = channels.findIndex((c) => c.id === state.selected[server])
        const next =
          channels[(index + (action === 'next' ? 1 : -1) + channels.length) % channels.length]
        if (next) state.select(server, next.id)
      }
    }
    window.addEventListener('keydown', shortcut)
    return () => window.removeEventListener('keydown', shortcut)
  }, [])
  useEffect(() => {
    if (globalPtt || !preferences.binding || !canTalk) return
    let bindingHeld = false
    const matches = (event: KeyboardEvent | PointerEvent) =>
      'code' in event
        ? event.code === preferences.binding
        : `Mouse${event.button}` === preferences.binding
    const down = (event: KeyboardEvent | PointerEvent) => {
      if (
        event.defaultPrevented ||
        !matches(event) ||
        ('repeat' in event && event.repeat) ||
        document.querySelector('dialog[open]')
      )
        return
      if (event.target instanceof Element) {
        if (event.target.closest('input,textarea,select,[contenteditable="true"]')) return
        const activatesControl =
          'key' in event ? event.key === ' ' || event.key === 'Enter' : event.button === 0
        if (activatesControl && event.target.closest('button,summary,a[href]')) return
      }
      event.preventDefault()
      bindingHeld = true
      ptt(true)
    }
    const up = (event: KeyboardEvent | PointerEvent) => {
      if (bindingHeld && matches(event)) {
        bindingHeld = false
        ptt(false)
      }
    }
    const cancel = () => {
      if (bindingHeld && preferences.binding.startsWith('Mouse')) {
        bindingHeld = false
        ptt(false)
      }
    }
    window.addEventListener('keydown', down)
    window.addEventListener('keyup', up)
    window.addEventListener('pointerdown', down)
    window.addEventListener('pointerup', up)
    window.addEventListener('pointercancel', cancel)
    return () => {
      ptt(false)
      window.removeEventListener('keydown', down)
      window.removeEventListener('keyup', up)
      window.removeEventListener('pointerdown', down)
      window.removeEventListener('pointerup', up)
      window.removeEventListener('pointercancel', cancel)
    }
  }, [globalPtt, preferences.binding, canTalk])
  function gate(open: boolean) {
    ptt(open && canTalk)
  }
  if (!call && !error && !notice && !settings) return null
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
      {recovery && (
        <section className="voice-dock recovery-dock" aria-label="Voice controls">
          <MicOff size={20} aria-hidden="true" />
          <div className="recovery-room" role="status">
            <strong title={recovery.name}>
              {server?.status.kind === 'reconnecting'
                ? 'Voice paused'
                : recovery.phase === 'waiting'
                  ? 'Rejoining voice'
                  : joining?.serverId === recovery.serverId &&
                      joining.channelId === recovery.channelId
                    ? 'Joining voice'
                    : !channel?.canJoin
                      ? 'Room unavailable'
                      : 'Couldn’t rejoin voice'}{' '}
              · {channel?.name ?? recovery.name}
            </strong>
            <span>
              {server?.status.kind === 'connected' && recovery.phase === 'waiting'
                ? 'Checking microphone status…'
                : 'Your microphone is off.'}
            </span>
          </div>
          {recovery.phase === 'rejoin' && channel?.canJoin && (
            <button
              className="button primary"
              disabled={busy || server?.status.kind !== 'connected'}
              onClick={() =>
                void runVoice(
                  () => joinVoice(recovery.serverId, recovery.channelId, recovery),
                  recovery,
                )
              }
            >
              Rejoin voice
            </button>
          )}
          {recovery.phase === 'rejoin' ? (
            <button
              className="button secondary"
              disabled={busy}
              onClick={() => useVoiceStore.setState({ recovery: null, error: '' })}
            >
              Dismiss
            </button>
          ) : (
            <button
              className="button call-leave"
              disabled={busy}
              onClick={() =>
                void runVoice(async () => {
                  try {
                    await leaveVoice()
                  } finally {
                    useVoiceStore.setState({ recovery: null })
                  }
                })
              }
            >
              <PhoneOff size={16} /> Leave voice
            </button>
          )}
        </section>
      )}
      {!recovery && active && (
        <section className="voice-dock" aria-label="Voice controls">
          <details className="call-identity">
            <summary title="Call details">
              <span className="call-status">
                <span
                  className={`quality-dot ${server?.status.kind === 'connected' ? (stats && stats.rttMs > 300 ? 'poor' : stats && stats.rttMs > 150 ? 'fair' : 'good') : 'poor'}`}
                  aria-hidden="true"
                />
                {server?.status.kind === 'connected'
                  ? active.ready
                    ? 'In voice'
                    : 'Rejoining voice'
                  : server?.status.kind === 'reconnecting'
                    ? 'Reconnecting'
                    : 'Offline'}
                <ChevronDown size={14} />
              </span>
              <strong title={channel?.name}>{channel?.name ?? 'Voice'}</strong>
            </summary>
            <div className="call-info">
              <span>
                {server?.label} / {channel?.name ?? 'Voice'} · {channel?.profile ?? 'voice'}
              </span>
              {own && (
                <span>
                  {own.displayName || own.username} · @{own.username}
                </span>
              )}
              <span>
                {stats
                  ? `${Math.round(stats.encoderBitrate / 1000)} kbps · ${stats.rttMs} ms network RTT`
                  : 'Measuring connection…'}
              </span>
            </div>
          </details>
          <StreamControls
            key={`${active.serverId}:${active.channelId}`}
            server={active.serverId}
            channel={active.channelId}
            connected={server?.status.kind === 'connected'}
          />
          <button
            className={`button ${mic.transmitting ? 'primary' : 'secondary'}`}
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
            title={canTalk ? 'Hold mouse, Space or Enter while focused' : mic.guidance}
          >
            {mic.label}
          </button>
          <div className="call-actions">
            <button
              className="button secondary"
              aria-pressed={active.muted}
              disabled={busy || server?.status.kind !== 'connected'}
              onClick={() => void runVoice(() => voiceControl('mute', !active.muted))}
            >
              {active.muted ? <MicOff size={16} /> : <Mic size={16} />}
              {active.muted ? 'Unmute' : 'Mute'}
            </button>
            <button
              className="button secondary"
              aria-pressed={active.deafened}
              disabled={busy || server?.status.kind !== 'connected'}
              onClick={() => void runVoice(() => voiceControl('deafen', !active.deafened))}
            >
              {active.deafened ? <HeadphoneOff size={16} /> : <Headphones size={16} />}
              {active.deafened ? 'Undeafen' : 'Deafen'}
            </button>
            <button
              className="button secondary"
              aria-label="Audio settings"
              title="Personal audio settings"
              onClick={(event) => {
                event.currentTarget.focus()
                setSettings(true)
              }}
            >
              <SlidersHorizontal size={16} /> Audio
            </button>
            <button
              className="button call-leave"
              disabled={busy}
              onClick={() => void runVoice(leaveVoice)}
            >
              <PhoneOff size={16} /> Leave voice
            </button>
          </div>
          {route && (
            <p className="call-route" role="status">
              {route}
            </p>
          )}
        </section>
      )}
      {settings && <AudioSettings close={() => setSettings(false)} />}
    </div>
  )
}
