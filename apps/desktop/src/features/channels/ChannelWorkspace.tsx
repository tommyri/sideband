import { ChevronDown } from 'lucide-react'
import { ActionMenu } from '../../components/ActionMenu'
import { disconnect } from '../../lib/api'
import { useStreamPolling } from '../stream/store'
import { MemberManager } from '../admin/MemberManager'
import { SearchPanel } from '../search/SearchPanel'
import { ServerSettings } from '../admin/ServerSettings'
import { RoleManager } from '../admin/RoleManager'
import { ChannelManager } from '../admin/ChannelManager'
import { WebhookManager } from '../admin/WebhookManager'
import { AuditLog } from '../admin/AuditLog'
import { InviteManager } from '../admin/InviteManager'
import { DmList } from '../dm/DmList'
import { DmDialog, type DmMode } from '../dm/DmDialog'
import { DmHeader } from '../dm/DmHeader'
import { dmName } from '../dm/model'
import { Chat } from '../chat/Chat'
import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { useAppStore } from '../../store/app-store'
import type { ChannelView } from '../../lib/generated/ChannelView'
import type { ServerView } from '../../lib/generated/ServerView'
import type { ConnectionStatus } from '../../lib/generated/ConnectionStatus'
import { useChannelMicrophone } from '../voice/microphone'
import { shortcutLabel } from '../voice/shortcuts'
import { refreshVoice, runVoice, useVoiceStore } from '../voice/store'
import { joinVoice, leaveVoice, memberAction } from '../voice/api'
import { MemberRow } from './MemberRow'
import { MemberMenu } from './MemberMenu'
import { AudioSettings } from '../settings/audio/AudioSettings'
import { ServerHome } from '../onboarding/ServerHome'
import { ServerNotices } from '../onboarding/ServerNotices'

function MicrophoneGuidance({
  server,
  view,
  channel,
  connection,
}: {
  server: string
  view: ServerView
  channel: ChannelView
  connection?: ConnectionStatus['kind']
}) {
  const mic = useChannelMicrophone(server, view, channel, connection)
  return <p>{mic.guidance}</p>
}

function visibleChannels(channels: ChannelView[], collapsed: Set<string>) {
  const byId = new Map(channels.map((c) => [c.id, c]))
  return channels.flatMap((channel) => {
    let parent = channel.parentId
    let depth = 0
    const seen = new Set([channel.id])
    while (parent && byId.has(parent) && !seen.has(parent)) {
      if (collapsed.has(parent)) return []
      seen.add(parent)
      depth++
      parent = byId.get(parent)?.parentId ?? null
    }
    return [{ channel, depth }]
  })
}
export function ChannelWorkspace({ server }: { server: string }) {
  useStreamPolling(server)
  const view = useVoiceStore((s) => s.views[server])
  const selected = useVoiceStore((s) => s.selected[server])
  const select = useVoiceStore((s) => s.select)
  const busy = useVoiceStore((s) => s.busy)
  const active = useVoiceStore((s) => s.active)
  const status = useAppStore((s) => s.servers.find((item) => item.id === server)?.status)
  const label = useAppStore((s) => s.servers.find((item) => item.id === server)?.label)
  const [collapsed, setCollapsed] = useState(new Set<string>())
  const [menu, setMenu] = useState<string | null>(null)
  const [audioSettings, setAudioSettings] = useState(false)
  const [searchOpen, setSearchOpen] = useState(false)
  const [details, setDetails] = useState(false)
  const navigationButton = useRef<HTMLButtonElement>(null)
  const previousDetails = useRef(details)
  useEffect(() => {
    if (previousDetails.current !== details) navigationButton.current?.focus()
    previousDetails.current = details
  }, [details])
  const [membersOpen, setMembersOpen] = useState(false)
  const [serverSettingsOpen, setServerSettingsOpen] = useState(false)
  const [rolesOpen, setRolesOpen] = useState(false)
  const [channelsOpen, setChannelsOpen] = useState(false)
  const [webhooksOpen, setWebhooksOpen] = useState(false)
  const [auditOpen, setAuditOpen] = useState(false)
  const [invitesOpen, setInvitesOpen] = useState(false)
  const [dmDialog, setDmDialog] = useState<DmMode | null>(null)
  const [dmWarning, setDmWarning] = useState('')
  const openMenu = useCallback((id: string) => setMenu(id), [])
  useEffect(() => {
    void refreshVoice(server)
  }, [server])
  const members = useMemo(() => new Map(view?.members.map((m) => [m.id, m])), [view?.members])
  const participantsByUser = useMemo(
    () => new Map(view?.participants.map((participant) => [participant.userId, participant])),
    [view?.participants],
  )
  const groups = useMemo(() => {
    const groups = new Map<
      string,
      {
        name: string
        color: string | null
        position: number
        users: NonNullable<typeof view>['members']
      }
    >()
    for (const member of view?.members ?? []) {
      const key = `${member.rolePosition}:${member.roleName}`
      if (!groups.has(key))
        groups.set(key, {
          name: member.roleName,
          color: member.roleColor,
          position: member.rolePosition,
          users: [],
        })
      groups.get(key)!.users.push(member)
    }
    return [...groups.values()].sort((a, b) => b.position - a.position)
  }, [view?.members])
  if (details)
    return (
      <section className="live-workspace details-view">
        <button
          ref={navigationButton}
          className="button secondary"
          onClick={() => setDetails(false)}
        >
          Back to channels
        </button>
        <ServerHome />
      </section>
    )
  const connected = status?.kind === 'connected'
  const serverHeader = (
    <header>
      <h2>
        <ActionMenu
          ref={navigationButton}
          className="server-menu-trigger"
          label={`Server menu for ${label ?? 'server'}`}
          actions={[
            { label: 'Server details', onSelect: () => setDetails(true) },
            { label: 'Audio settings', onSelect: () => setAudioSettings(true) },
            {
              label: 'Members & moderation',
              hidden: !view?.canModerateMembers,
              disabled: !connected,
              onSelect: () => setMembersOpen(true),
            },
            {
              label: 'Server settings',
              hidden: !view?.canManageServer,
              disabled: !connected,
              onSelect: () => setServerSettingsOpen(true),
            },
            {
              label: 'Roles and permissions',
              hidden: !(view?.canManageRoles || view?.canManageChannels),
              disabled: !connected,
              onSelect: () => setRolesOpen(true),
            },
            {
              label: 'Manage channels',
              hidden: !view?.canManageChannels,
              disabled: !connected,
              onSelect: () => setChannelsOpen(true),
            },
            {
              label: 'Webhooks',
              hidden: !view?.canManageWebhooks,
              disabled: !connected,
              onSelect: () => setWebhooksOpen(true),
            },
            {
              label: 'Audit log',
              hidden: !view?.canViewAuditLog,
              disabled: !connected,
              onSelect: () => setAuditOpen(true),
            },
            {
              label: 'Invites',
              hidden: !(view?.canCreateInvites || view?.canManageInvites),
              disabled: !connected,
              onSelect: () => setInvitesOpen(true),
            },
            {
              label: 'Disconnect from server',
              divider: true,
              disabled: busy,
              onSelect: () => void runVoice(() => disconnect(server)),
            },
          ]}
        >
          <span>{label ?? 'Server'}</span>
          <ChevronDown size={16} aria-hidden="true" />
        </ActionMenu>
      </h2>
    </header>
  )
  const audioDialog = audioSettings && <AudioSettings close={() => setAudioSettings(false)} />
  if (!view)
    return (
      <section className="live-workspace">
        {audioDialog}
        <aside className="live-channels" aria-label="Channel tree">
          {serverHeader}
        </aside>
        <main className="live-channel-main">
          <p role="status">Loading channels…</p>
          <button className="button secondary" onClick={() => void refreshVoice(server)}>
            Retry
          </button>
        </main>
      </section>
    )
  const current = view.channels.find(
    (c) => c.id === (selected ?? (active?.serverId === server ? active.channelId : undefined)),
  )
  const currentDm = view.directMessages.find((dm) => dm.id === selected)
  const chatChannel = currentDm ?? (current?.kind === 2 ? current : undefined)
  const title = currentDm ? dmName(currentDm, members, view.ownUserId) : current?.name
  const chosenMember = view.members.find((m) => m.id === menu)
  const join = (channel: ChannelView) => {
    select(server, channel.id)
    if (channel.kind === 3) void runVoice(() => joinVoice(server, channel.id))
  }
  return (
    <section className={`live-workspace ${chatChannel ? 'has-chat' : ''}`}>
      {searchOpen && connected && (
        <SearchPanel
          key={`${server}:${view.ownUserId}`}
          server={server}
          view={view}
          close={() => setSearchOpen(false)}
        />
      )}
      {dmDialog && (dmDialog === 'create' || currentDm) && (
        <DmDialog
          key={`${dmDialog}:${currentDm?.id ?? ''}`}
          server={server}
          view={view}
          conversation={currentDm}
          mode={dmDialog}
          connected={connected}
          close={() => setDmDialog(null)}
          complete={(result) => {
            const leaving = dmDialog === 'leave'
            setDmDialog(null)
            setDmWarning(result.warning ?? '')
            select(server, leaving ? '' : result.channelId)
            void refreshVoice(server)
          }}
        />
      )}
      {invitesOpen && (view.canCreateInvites || view.canManageInvites) && (
        <InviteManager
          key={`${server}:${view.ownUserId}`}
          server={server}
          view={view}
          connected={connected}
          close={() => setInvitesOpen(false)}
        />
      )}
      {membersOpen && view.canModerateMembers && (
        <MemberManager
          key={`${server}:${view.ownUserId}`}
          server={server}
          connected={connected}
          permitted={view.canModerateMembers}
          close={() => setMembersOpen(false)}
        />
      )}
      {serverSettingsOpen && view.canManageServer && (
        <ServerSettings
          server={server}
          connected={connected}
          permitted={view.canManageServer}
          close={() => setServerSettingsOpen(false)}
        />
      )}
      {rolesOpen && (view.canManageRoles || view.canManageChannels) && (
        <RoleManager
          server={server}
          view={view}
          connected={connected}
          close={() => setRolesOpen(false)}
        />
      )}
      {channelsOpen && view.canManageChannels && (
        <ChannelManager
          server={server}
          view={view}
          connected={connected}
          close={() => setChannelsOpen(false)}
        />
      )}
      {webhooksOpen && view.canManageWebhooks && (
        <WebhookManager
          server={server}
          view={view}
          connected={connected}
          close={() => setWebhooksOpen(false)}
        />
      )}
      {auditOpen && view.canViewAuditLog && (
        <AuditLog
          key={`${server}:${view.ownUserId}`}
          server={server}
          view={view}
          connected={connected}
          close={() => setAuditOpen(false)}
        />
      )}
      {audioDialog}
      <aside className="live-channels" aria-label="Channel tree">
        {serverHeader}
        <div className="live-channel-scroll">
          <DmList
            server={server}
            view={view}
            selected={selected}
            connected={connected}
            select={(id) => {
              select(server, id)
              setDmDialog(null)
              setDmWarning('')
            }}
            create={() => setDmDialog('create')}
          />
          {visibleChannels(view.channels, collapsed).map(({ channel, depth }) => {
            const participants = view.participants.filter((p) => p.channelId === channel.id)
            return (
              <div
                key={channel.id}
                style={{ paddingLeft: Math.min(depth, 5) * 12 }}
                onDragOver={(event) => {
                  if (channel.kind === 3 && connected) event.preventDefault()
                }}
                onDrop={(event) => {
                  event.preventDefault()
                  try {
                    const data = JSON.parse(
                      event.dataTransfer.getData('application/x-socialspeak-member'),
                    ) as { server: string; user: string }
                    if (
                      data.server === server &&
                      members.get(data.user)?.canMove &&
                      channel.kind === 3
                    )
                      void runVoice(() =>
                        memberAction({
                          serverId: server,
                          userId: data.user,
                          action: 'move',
                          channelId: channel.id,
                          enabled: null,
                          volume: null,
                          reason: null,
                        }),
                      )
                  } catch {
                    /* ignore other drag formats */
                  }
                }}
              >
                {channel.kind === 1 ? (
                  <button
                    className="live-category"
                    aria-expanded={!collapsed.has(channel.id)}
                    onClick={() =>
                      setCollapsed((old) => {
                        const next = new Set(old)
                        if (next.has(channel.id)) next.delete(channel.id)
                        else next.add(channel.id)
                        return next
                      })
                    }
                  >
                    {collapsed.has(channel.id) ? '▸' : '▾'} {channel.name}
                  </button>
                ) : (
                  <button
                    className={`live-channel ${current?.id === channel.id ? 'selected' : ''}`}
                    aria-label={`${channel.kind === 3 ? 'Join voice channel' : 'Open text channel'} ${channel.name}`}
                    disabled={busy || !connected || (channel.kind === 3 && !channel.canJoin)}
                    title={
                      channel.kind === 3 && !channel.canJoin
                        ? 'Requires VIEW_CHANNEL and CONNECT'
                        : undefined
                    }
                    onClick={() => {
                      select(server, channel.id)
                      if (localStorage.getItem('ss.doubleClickJoin') !== 'true') join(channel)
                    }}
                    onDoubleClick={() => {
                      if (localStorage.getItem('ss.doubleClickJoin') === 'true') join(channel)
                    }}
                  >
                    <span>{channel.kind === 3 ? '◖' : '#'}</span> {channel.name}
                    {channel.unread && (
                      <span className="unread-marker" aria-label="Unread messages">
                        ●
                      </span>
                    )}
                    {channel.mentions > 0 && (
                      <span className="unread-count" aria-label={`${channel.mentions} mentions`}>
                        {channel.mentions}
                      </span>
                    )}
                    {channel.kind === 3 && (
                      <small>
                        {participants.length}
                        {channel.userLimit != null ? `/${channel.userLimit}` : ''}
                      </small>
                    )}
                  </button>
                )}
                {participants.map((participant) => {
                  const member = members.get(participant.userId)
                  return member ? (
                    <MemberRow
                      key={participant.connectionId}
                      server={server}
                      member={member}
                      participant={participant}
                      onMenu={openMenu}
                    />
                  ) : null
                })}
              </div>
            )
          })}
          {view.channels.length === 0 && <p>No channels are visible to your account.</p>}
        </div>
      </aside>
      <main className={`live-channel-main ${chatChannel ? 'text-workspace' : ''}`}>
        <header>
          <p className="eyebrow">
            {currentDm
              ? 'Direct messages'
              : current?.kind === 3
                ? 'Voice channel'
                : 'Your community'}
          </p>
          <h1>{title ?? 'Choose a channel'}</h1>
          <button
            className="button secondary"
            disabled={!connected}
            onClick={() => setSearchOpen(true)}
          >
            Search messages
          </button>
          {currentDm && (
            <DmHeader
              conversation={currentDm}
              view={view}
              connected={connected}
              manage={setDmDialog}
            />
          )}
          {dmWarning && <p role="alert">{dmWarning}</p>}
          {current?.kind === 3 && (
            <p>
              {current.profile} profile ·{' '}
              {view.participants.filter((p) => p.channelId === current.id).length} in voice
            </p>
          )}
        </header>
        {status?.kind === 'reconnecting' && (
          <p role="status" className="connection-banner">
            Reconnecting to {label} (attempt {status.attempt})…
          </p>
        )}
        <ServerNotices server={server} />
        {current?.kind === 3 ? (
          <>
            {active?.serverId === server && active.channelId === current.id ? (
              <button
                className="button secondary"
                onClick={() => void runVoice(leaveVoice)}
                disabled={busy}
              >
                Leave voice
              </button>
            ) : (
              <button
                className="button primary"
                disabled={busy || !connected || !current.canJoin}
                onClick={() => join(current)}
              >
                Join voice
              </button>
            )}
            <MicrophoneGuidance
              server={server}
              view={view}
              channel={current}
              connection={status?.kind}
            />
            <div className="voice-stage">
              {view.participants
                .filter((p) => p.channelId === current.id)
                .map((participant) => {
                  const member = members.get(participant.userId)
                  return member ? (
                    <MemberRow
                      key={participant.connectionId}
                      server={server}
                      member={member}
                      participant={participant}
                      onMenu={openMenu}
                    />
                  ) : null
                })}
            </div>
          </>
        ) : chatChannel ? (
          <Chat
            key={`${server}:${chatChannel.id}`}
            server={server}
            channel={chatChannel.id}
            name={title ?? 'Conversation'}
            connected={connected}
          />
        ) : (
          <p>Select a channel to join the conversation.</p>
        )}
        {!chatChannel && (
          <details className="voice-ui-settings">
            <summary>Interaction settings</summary>
            <label>
              <input
                type="checkbox"
                defaultChecked={localStorage.getItem('ss.doubleClickJoin') === 'true'}
                onChange={(e) =>
                  localStorage.setItem('ss.doubleClickJoin', String(e.target.checked))
                }
              />{' '}
              Double-click voice channels to join
            </label>
            <p>
              Mute: {shortcutLabel('mute')} · Deafen: {shortcutLabel('deafen')} · Previous channel:{' '}
              {shortcutLabel('previous')} · Next channel: {shortcutLabel('next')}. These shortcuts
              work while the app is focused.
            </p>
            <p>
              Choose your devices and talking mode in Audio settings. Open Global shortcuts there to
              set up push-to-talk outside the app.
            </p>
          </details>
        )}
      </main>
      <aside className="live-members" aria-label="Members">
        {groups.map((group) => (
          <section key={`${group.position}:${group.name}`}>
            <h3 style={{ color: group.color ?? undefined }}>
              {group.name} — {group.users.length}
            </h3>
            {group.users.map((member) => (
              <MemberRow
                key={member.id}
                server={server}
                member={member}
                participant={participantsByUser.get(member.id)}
                onMenu={openMenu}
              />
            ))}
          </section>
        ))}
      </aside>
      {chosenMember && (
        <MemberMenu
          connected={connected}
          server={server}
          member={chosenMember}
          view={view}
          close={() => setMenu(null)}
        />
      )}
    </section>
  )
}
