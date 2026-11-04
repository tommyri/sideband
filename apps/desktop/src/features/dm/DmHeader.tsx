import type { ServerView } from '../../lib/generated/ServerView'
import type { DmView } from '../../lib/generated/DmView'
import { canAddMembers } from './model'
import type { DmMode } from './DmDialog'
export function DmHeader({
  conversation,
  view,
  connected,
  manage,
}: {
  conversation: DmView
  view: ServerView
  connected: boolean
  manage: (mode: DmMode) => void
}) {
  const members = new Map(view.members.map((m) => [m.id, m]))
  return (
    <div className="dm-header">
      <details>
        <summary>{conversation.memberIds.length} participants</summary>
        <ul>
          {conversation.memberIds.map((id) => (
            <li key={id}>
              {members.get(id)?.displayName || 'Former member'}{' '}
              <small>@{members.get(id)?.username ?? id}</small>
            </li>
          ))}
        </ul>
      </details>
      {conversation.kind === 5 && (
        <div className="dm-header-actions">
          <button
            className="button secondary"
            disabled={!connected || !canAddMembers(conversation, view.ownUserId)}
            title="Only the creator can add people, up to 10 total."
            onClick={() => manage('add')}
          >
            Add members
          </button>
          <button
            className="button secondary"
            disabled={!connected}
            onClick={() => manage('rename')}
          >
            Rename
          </button>
          <button
            className="button secondary"
            disabled={!connected}
            onClick={() => manage('leave')}
          >
            Leave group
          </button>
        </div>
      )}
    </div>
  )
}
