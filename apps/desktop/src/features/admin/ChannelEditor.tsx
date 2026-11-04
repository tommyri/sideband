import { useState } from 'react'
import type { AdminChannel } from '../../lib/generated/AdminChannel'
import type { ChannelFields } from '../../lib/generated/ChannelFields'
import type { ChannelAdminSnapshot } from '../../lib/generated/ChannelAdminSnapshot'
import type { ChannelAdminAction } from '../../lib/generated/ChannelAdminAction'
import { channelKind, validFields } from './channels'
import { errorText } from '../chat/api'
export function ChannelEditor({
  original,
  snapshot,
  busy,
  save,
  cancel,
}: {
  original: AdminChannel | null
  snapshot: ChannelAdminSnapshot
  busy: boolean
  save: (action: ChannelAdminAction) => Promise<void>
  cancel: () => void
}) {
  const [kind, setKind] = useState(original?.kind ?? 2)
  const [parent, setParent] = useState('')
  const [fields, setFields] = useState<ChannelFields>({
    name: original?.name ?? '',
    topic: original?.topic ?? '',
    profile: original?.profile || 1,
    bitrate: original?.bitrate ?? null,
    userLimit: original?.userLimit ?? null,
  })
  const [error, setError] = useState('')
  const current = snapshot.channels.find((channel) => channel.id === original?.id)
  const stale =
    original !== null &&
    (!current ||
      ['name', 'topic', 'kind', 'profile', 'bitrate', 'userLimit'].some(
        (field) => current[field as keyof AdminChannel] !== original[field as keyof AdminChannel],
      ))
  const profile = snapshot.profiles.find((p) => p.id === fields.profile)
  const submit = () => {
    try {
      const checked = validFields(fields, kind, snapshot.profiles)
      if (!original && parent && !snapshot.channels.some((c) => c.id === parent && c.kind === 1))
        throw new Error('The selected category is no longer available.')
      setError('')
      void save(
        original
          ? { kind: 'edit', channel: original.id, fields: checked }
          : {
              kind: 'create',
              channelKind: kind,
              parentId: kind === 1 ? null : parent || null,
              fields: checked,
            },
      )
    } catch (e) {
      setError(errorText(e))
    }
  }
  return (
    <section className="channel-editor" aria-label={original ? 'Edit channel' : 'New channel'}>
      <h3>{original ? `Edit ${original.name}` : 'New channel'}</h3>
      {stale && (
        <p role="alert">
          This channel changed or was removed on the server. Cancel and reopen the editor to load
          its latest settings.
        </p>
      )}
      <form
        onSubmit={(event) => {
          event.preventDefault()
          submit()
        }}
      >
        <fieldset disabled={busy || stale}>
          <label>
            Name
            <input
              autoFocus
              value={fields.name}
              onChange={(e) => setFields({ ...fields, name: e.target.value })}
            />
          </label>
          <label>
            Type
            <select
              value={kind}
              disabled={original !== null}
              onChange={(e) => {
                setKind(Number(e.target.value))
                setParent('')
              }}
            >
              {[1, 2, 3].map((value) => (
                <option key={value} value={value}>
                  {channelKind(value)}
                </option>
              ))}
            </select>
          </label>
          {original ? (
            <small>
              Channel type is set at creation. Use Move to change its category or order.
            </small>
          ) : (
            kind !== 1 && (
              <label>
                Category
                <select value={parent} onChange={(e) => setParent(e.target.value)}>
                  <option value="">Root (no category)</option>
                  {snapshot.channels
                    .filter((c) => c.kind === 1)
                    .map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                </select>
              </label>
            )
          )}
          <label className="channel-wide">
            Topic
            <textarea
              value={fields.topic}
              onChange={(e) => setFields({ ...fields, topic: e.target.value })}
              rows={3}
            />
          </label>
          {kind === 3 && (
            <>
              <label>
                Voice profile
                <select
                  value={fields.profile}
                  onChange={(e) =>
                    setFields({ ...fields, profile: Number(e.target.value), bitrate: null })
                  }
                >
                  {snapshot.profiles.map((p) => (
                    <option key={p.id} value={p.id}>
                      {p.name}
                    </option>
                  ))}
                </select>
              </label>
              <label>
                User limit
                <input
                  type="number"
                  min={0}
                  max={99}
                  step={1}
                  placeholder="Unlimited"
                  value={fields.userLimit ?? ''}
                  onChange={(e) =>
                    setFields({
                      ...fields,
                      userLimit: e.target.value === '' ? null : Number(e.target.value),
                    })
                  }
                />
                <small>0 or blank means unlimited.</small>
              </label>
              {profile && (
                <div className="channel-wide">
                  <label className="channel-checkbox">
                    <input
                      type="checkbox"
                      checked={fields.bitrate === null}
                      onChange={(e) =>
                        setFields({
                          ...fields,
                          bitrate: e.target.checked ? null : profile.defaultBitrate,
                        })
                      }
                    />
                    Use profile default ({profile.defaultBitrate / 1000} kb/s)
                  </label>
                  <label>
                    Bitrate: {(fields.bitrate ?? profile.defaultBitrate) / 1000} kb/s
                    <input
                      aria-label="Bitrate"
                      type="range"
                      min={profile.minBitrate}
                      max={profile.maxBitrate}
                      step={1}
                      disabled={
                        fields.bitrate === null || profile.minBitrate === profile.maxBitrate
                      }
                      value={fields.bitrate ?? profile.defaultBitrate}
                      onChange={(e) => setFields({ ...fields, bitrate: Number(e.target.value) })}
                    />
                  </label>
                  <small>
                    {profile.minBitrate / 1000}–{profile.maxBitrate / 1000} kb/s
                  </small>
                </div>
              )}
            </>
          )}
        </fieldset>
        {error && <p role="alert">{error}</p>}
        <div className="channel-tools">
          <button className="button primary" disabled={busy || stale}>
            {original ? 'Save changes' : 'Create channel'}
          </button>
          <button type="button" className="button secondary" disabled={busy} onClick={cancel}>
            Cancel
          </button>
        </div>
      </form>
    </section>
  )
}
