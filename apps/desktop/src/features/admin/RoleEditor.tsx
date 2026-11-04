import { useState } from 'react'
import type { AdminRole } from '../../lib/generated/AdminRole'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
import type { RoleAction } from '../../lib/generated/RoleAction'
import { has, permissions, toggle } from './roles'
export function RoleEditor({
  original,
  data,
  busy,
  save,
  cancel,
}: {
  original: AdminRole | null
  data: RoleSnapshot
  busy: boolean
  save: (action: RoleAction) => Promise<void>
  cancel: () => void
}) {
  const [name, setName] = useState(original?.name ?? '')
  const [color, setColor] = useState(
    original?.color == null ? '' : `#${original.color.toString(16).padStart(6, '0')}`,
  )
  const [mask, setMask] = useState(original?.permissions ?? '0')
  const current = data.roles.find((r) => r.id === original?.id)
  const stale =
    !!original &&
    (!current ||
      ['name', 'color', 'permissions'].some(
        (k) => current[k as keyof AdminRole] !== original[k as keyof AdminRole],
      ))
  const disabled = busy || !data.manageRoles || (!!original && !current?.editable) || stale
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        if (disabled) return
        const fields = {
          name: name.trim(),
          color: color ? parseInt(color.slice(1), 16) : null,
          permissions: mask,
        }
        void save(
          original ? { kind: 'edit', role: original.id, fields } : { kind: 'create', fields },
        )
      }}
    >
      <h3>{original ? `Edit ${original.name}` : 'Create role'}</h3>
      {stale && (
        <p role="alert">This role changed on the server. Cancel and reopen it before saving.</p>
      )}
      <fieldset disabled={disabled} className="role-fields">
        <label>
          Name
          <input required maxLength={100} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label>
          Role color
          <input
            type="color"
            value={color || '#94a3b8'}
            onChange={(e) => setColor(e.target.value)}
          />
        </label>
        <label>
          <input
            type="checkbox"
            checked={!color}
            disabled={!data.supportsV2 && !!original}
            onChange={(e) => setColor(e.target.checked ? '' : '#94a3b8')}
          />
          Use default color
        </label>
        {['Server', 'Moderation', 'Text and voice'].map((group) => (
          <fieldset key={group}>
            <legend>{group}</legend>
            {permissions.map(
              ([label, section, hint], bit) =>
                section === group && (
                  <label className="permission-choice" key={label}>
                    <input
                      type="checkbox"
                      checked={has(mask, bit)}
                      disabled={bit === 0 && !data.administrator}
                      onChange={(e) => setMask(toggle(mask, bit, e.target.checked))}
                    />
                    <span>
                      {label}
                      <small>{hint}</small>
                    </span>
                  </label>
                ),
            )}
          </fieldset>
        ))}
        {has(mask, 0) && (
          <p role="note">
            Administrator grants unrestricted access, including access to every guild channel.
          </p>
        )}
        <button className="button primary" disabled={!name.trim() || [...name.trim()].length > 100}>
          Save role
        </button>
      </fieldset>
      <button type="button" className="button secondary" disabled={busy} onClick={cancel}>
        Cancel
      </button>
    </form>
  )
}
