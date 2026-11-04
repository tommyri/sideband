import { useEffect, useState } from 'react'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
import type { RoleAction } from '../../lib/generated/RoleAction'
import type { ChannelView } from '../../lib/generated/ChannelView'
import { errorText, onChatEvent } from '../chat/api'
import {
  channelBits,
  permissions,
  roleOverrides,
  overrideValue,
  setOverride,
  type Override,
} from './roles'
export function ChannelOverrides({
  server,
  channels,
  data,
  busy,
  save,
}: {
  server: string
  channels: ChannelView[]
  data: RoleSnapshot
  busy: boolean
  save: (action: RoleAction) => Promise<void>
}) {
  const [channel, setChannel] = useState('')
  const [role, setRole] = useState('')
  const [revision, setRevision] = useState(0)
  return (
    <section>
      <h3>Channel overrides</h3>
      <p>
        Inherit makes no change for this role. Overrides apply from lowest to highest role; higher
        roles win. Administrators bypass overrides. Category overrides do not carry over to child
        channels.
      </p>
      <label>
        Channel
        <select disabled={busy} value={channel} onChange={(e) => setChannel(e.target.value)}>
          <option value="">Choose a channel</option>
          {channels
            .filter((c) => c.kind >= 1 && c.kind <= 3)
            .map((c) => (
              <option key={c.id} value={c.id}>
                {c.name}
              </option>
            ))}
        </select>
      </label>
      <label>
        Role
        <select disabled={busy} value={role} onChange={(e) => setRole(e.target.value)}>
          <option value="">Choose a role</option>
          {data.roles.map((r) => (
            <option key={r.id} value={r.id}>
              {r.name}
              {!r.editable ? ' (protected)' : ''}
            </option>
          ))}
        </select>
      </label>
      {channel && role && (
        <OverrideEditor
          key={`${channel}:${role}:${revision}`}
          server={server}
          channel={channel}
          role={role}
          enabled={
            data.manageChannels &&
            !!data.roles.find((r) => r.id === role)?.editable &&
            channels.some((c) => c.id === channel)
          }
          busy={busy}
          save={save}
          reload={() => setRevision((v) => v + 1)}
        />
      )}
    </section>
  )
}
function OverrideEditor({
  server,
  channel,
  role,
  enabled,
  busy,
  save,
  reload,
}: {
  server: string
  channel: string
  role: string
  enabled: boolean
  busy: boolean
  save: (action: RoleAction) => Promise<void>
  reload: () => void
}) {
  const [value, setValue] = useState<Override | null>(null)
  const [error, setError] = useState('')
  const [stale, setStale] = useState(false)
  useEffect(() => {
    let alive = true
    void roleOverrides(server, channel)
      .then((rows) => {
        if (alive)
          setValue(rows.find((r) => r.roleId === role) ?? { roleId: role, allow: '0', deny: '0' })
      })
      .catch((e) => {
        if (alive) setError(errorText(e))
      })
    const stop = onChatEvent((e) => {
      if (
        e.serverId === server &&
        ['channelsChanged', 'resynced', 'membersChanged'].includes(e.event.kind)
      )
        setStale(true)
    })
    return () => {
      alive = false
      stop()
    }
  }, [server, channel, role])
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        if (value && enabled && !stale && !busy)
          void save({ kind: 'override', channel, role, allow: value.allow, deny: value.deny })
      }}
    >
      {error && <p role="alert">{error}</p>}
      {stale && <p role="alert">Permissions may have changed. Reload before saving.</p>}
      {!value && !error && <p role="status">Loading saved overrides…</p>}
      {(error || stale) && (
        <button type="button" className="button secondary" disabled={busy} onClick={reload}>
          Reload overrides
        </button>
      )}
      {!enabled && <p>Your role cannot edit this override.</p>}
      <fieldset disabled={busy || !enabled || !value || stale}>
        {value &&
          channelBits.map((bit) => (
            <label className="override-choice" key={bit}>
              <span>
                {permissions[bit][0]}
                <small>{permissions[bit][2]}</small>
              </span>
              <select
                aria-label={permissions[bit][0]}
                value={overrideValue(value, bit)}
                onChange={(e) => setValue(setOverride(value, bit, e.target.value))}
              >
                <option value="inherit">Inherit</option>
                <option value="allow">Allow</option>
                <option value="deny">Deny</option>
              </select>
            </label>
          ))}
        <p>
          Permissions not shown here are preserved. Use role permissions for server-wide actions.
        </p>
        <button
          type="button"
          className="button secondary"
          onClick={() => {
            if (value)
              setValue(channelBits.reduce((v, bit) => setOverride(v, bit, 'inherit'), value))
          }}
        >
          Reset shown permissions to inherit
        </button>
        <button className="button primary">Save overrides</button>
      </fieldset>
    </form>
  )
}
