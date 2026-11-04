import { useState } from 'react'
import type { RoleMember } from '../../lib/generated/RoleMember'
import type { RoleSnapshot } from '../../lib/generated/RoleSnapshot'
import type { RoleAction } from '../../lib/generated/RoleAction'
export function MemberRoles({
  original,
  data,
  own,
  busy,
  save,
  cancel,
}: {
  original: RoleMember
  data: RoleSnapshot
  own: string
  busy: boolean
  save: (action: RoleAction) => Promise<void>
  cancel: () => void
}) {
  const [roles, setRoles] = useState(original.roleIds)
  const current = data.members.find((m) => m.id === original.id)
  const stale =
    !current || [...current.roleIds].sort().join() !== [...original.roleIds].sort().join()
  const disabled = busy || stale || !current?.editable || !data.manageRoles
  return (
    <form
      onSubmit={(e) => {
        e.preventDefault()
        if (!disabled) void save({ kind: 'assign', member: original.id, roles })
      }}
    >
      <h3>Roles for {original.name}</h3>
      {stale && <p role="alert">Assignments changed. Cancel and reopen this member.</p>}
      {own === original.id && (
        <p role="note">Changing your own roles can remove your administrative access.</p>
      )}
      <fieldset disabled={disabled}>
        {data.roles.map((r) => (
          <label className="permission-choice" key={r.id}>
            <input
              type="checkbox"
              checked={r.isDefault || roles.includes(r.id)}
              disabled={r.isDefault || !r.editable}
              onChange={(e) =>
                setRoles(e.target.checked ? [...roles, r.id] : roles.filter((id) => id !== r.id))
              }
            />
            {r.name}
            {r.isDefault ? ' (everyone, always applied)' : !r.editable ? ' (protected)' : ''}
          </label>
        ))}
        <button className="button primary">Save assignments</button>
      </fieldset>
      <button type="button" className="button secondary" disabled={busy} onClick={cancel}>
        Cancel
      </button>
    </form>
  )
}
