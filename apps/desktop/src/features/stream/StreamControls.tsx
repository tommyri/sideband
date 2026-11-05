import { useState } from 'react'
import { useVoiceStore } from '../voice/store'
import { useStreams, useStreamPolling } from './store'
import { SourcePicker } from './SourcePicker'
import * as api from './api'
export function StreamControls({
  server,
  channel,
  connected,
}: {
  server: string
  channel: string
  connected: boolean
}) {
  useStreamPolling(server)
  const view = useStreams((s) => s.views[server])
  const [picker, setPicker] = useState(false)
  const [error, setError] = useState('')
  const share = view?.share
  const active = share && share.state !== 'stopped'
  const allowed = connected && !!view?.canShare
  return (
    <div className="stream-controls">
      {active ? (
        <>
          <span role="status">
            {share.state === 'sharing'
              ? `You are streaming · ${share.viewers} watching`
              : share.state === 'starting'
                ? 'Starting screen share…'
                : 'Stopping screen share…'}
          </span>
          <small title={share.source}>{share.source}</small>
          <button
            className="button secondary"
            disabled={share.state === 'stopping'}
            onClick={() => void api.stop().catch((e) => setError(api.message(e)))}
          >
            Stop sharing
          </button>
        </>
      ) : (
        <button
          className="button secondary"
          disabled={!allowed}
          title={!allowed ? 'Join voice with STREAM permission to share' : undefined}
          onClick={() => setPicker(true)}
        >
          Share screen
        </button>
      )}
      {(error || share?.error) && <p role="alert">{error || share?.error}</p>}
      {picker && (
        <SourcePicker
          server={server}
          channel={channel}
          allowed={allowed}
          close={() => setPicker(false)}
        />
      )}
    </div>
  )
}
export function WatchStream({
  server,
  user,
  channel,
}: {
  server: string
  user: string
  channel: string
}) {
  const view = useStreams((s) => s.views[server])
  const voice = useVoiceStore((s) => s.active)
  const own = useVoiceStore((s) => s.views[server]?.ownUserId)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const track = view?.tracks.find((t) => t.user === user && t.channel === channel)
  if (!track) return null
  const watching = view?.watching.includes(track.track)
  const sameChannel = voice?.serverId === server && voice.channelId === channel
  async function watch() {
    if (!track || busy || !sameChannel) return
    setBusy(true)
    setError('')
    try {
      await api.watch(server, track.track)
    } catch (e) {
      setError(api.message(e))
    } finally {
      setBusy(false)
    }
  }
  return (
    <span className="stream-watch">
      <span className="stream-live">LIVE</span>
      {own !== user && (
        <button
          disabled={!sameChannel || watching || busy}
          title={!sameChannel ? 'Join this voice channel to watch' : undefined}
          onClick={() => void watch()}
        >
          {busy ? 'Opening…' : watching ? 'Watching' : 'Watch'}
        </button>
      )}
      {error && <small role="alert">{error}</small>}
    </span>
  )
}
