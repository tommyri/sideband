import { useState } from 'react'
import type { AdminChannel } from '../../lib/generated/AdminChannel'
import type { ChannelAdminAction } from '../../lib/generated/ChannelAdminAction'
export function ChannelMove({
  channel,
  channels,
  busy,
  save,
  cancel,
}: {
  channel: AdminChannel
  channels: AdminChannel[]
  busy: boolean
  save: (action: ChannelAdminAction) => Promise<void>
  cancel: () => void
}) {
  const [parent, setParent] = useState(channel.parentId ?? '')
  const [position, setPosition] = useState(channel.position + 1)
  const [error, setError] = useState('')
  const unavailable = !channels.some((c) => c.id === channel.id)
  return (
    <section className="channel-editor" aria-label="Move channel">
      <h3>Move {channel.name}</h3>
      {unavailable && <p role="alert">This channel is no longer available.</p>}
      <form
        onSubmit={(e) => {
          e.preventDefault()
          if (!Number.isInteger(position) || position < 1 || position > 2147483647) {
            setError('Position must be a positive whole number.')
            return
          }
          if (parent && !channels.some((c) => c.id === parent && c.kind === 1)) {
            setError('Category is no longer available.')
            return
          }
          setError('')
          void save({
            kind: 'move',
            channel: channel.id,
            parentId: parent || null,
            position: position - 1,
          })
        }}
      >
        <fieldset disabled={busy || unavailable}>
          <label>
            Category
            <select
              autoFocus
              disabled={channel.kind === 1}
              value={parent}
              onChange={(e) => {
                setParent(e.target.value)
                setPosition(1)
              }}
            >
              <option value="">Root (no category)</option>
              {channels
                .filter((c) => c.kind === 1 && c.id !== channel.id)
                .map((c) => (
                  <option key={c.id} value={c.id}>
                    {c.name}
                  </option>
                ))}
            </select>
          </label>
          <label>
            Position
            <input
              type="number"
              min={1}
              max={2147483647}
              step={1}
              value={position}
              onChange={(e) => setPosition(Number(e.target.value))}
            />
            <small>
              1 is first. Larger than the list places it last; hidden channels also count.
            </small>
          </label>
        </fieldset>
        {error && <p role="alert">{error}</p>}
        <div className="channel-tools">
          <button className="button primary" disabled={busy || unavailable}>
            Move channel
          </button>
          <button type="button" className="button secondary" disabled={busy} onClick={cancel}>
            Cancel
          </button>
        </div>
      </form>
    </section>
  )
}
