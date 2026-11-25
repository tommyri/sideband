import {
  ArrowLeft,
  ChevronDown,
  Hash,
  MessageCircle,
  PanelLeft,
  Search,
  SlidersHorizontal,
  Users,
  Volume2,
} from 'lucide-react'
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
import { ChatMembers } from './ChatMembers'
import { DmHeader } from '../dm/DmHeader'
import { dmName } from '../dm/model'
import { Chat } from '../chat/Chat'
import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { useAppStore } from '../../store/app-store'
import type { ChannelView } from '../../lib/generated/ChannelView'
import type { ParticipantView } from '../../lib/generated/ParticipantView'
import { VoiceRoomPreview } from './VoiceRoomPreview'
import { NoChannels, VoiceLobby } from './ServerLanding'
import { lastConversation, rememberConversation, workspaceDestination } from './destination'
import { shortcutLabel } from '../voice/shortcuts'
import { refreshVoice, runVoice, useVoiceStore } from '../voice/store'
import { memberAction } from '../voice/api'
import { MemberRow } from './MemberRow'
import { MemberMenu } from './MemberMenu'
import { AudioSettings } from '../settings/audio/AudioSettings'
import { ServerHome } from '../onboarding/ServerHome'
import { ServerNotices } from '../onboarding/ServerNotices'

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
  const current = view?.channels.find(
    (c) => c.id === (selected ?? (active?.serverId === server ? active.channelId : undefined)),
  )
  const currentDm = view?.directMessages.find((dm) => dm.id === selected)
  const chatChannel = currentDm ?? (current?.kind === 2 ? current : undefined)
  const conversationId = chatChannel?.id
  const conversationKind = currentDm ? 'dm' : 'channel'
  const user = view?.ownUserId
  useEffect(() => {
    if (user && conversationId)
      rememberConversation(server, user, { id: conversationId, kind: conversationKind })
  }, [server, user, conversationId, conversationKind])
  const summary = useAppStore((s) => s.servers.find((item) => item.id === server))
  const status = summary?.status
  const label = summary?.label
  const recoveringVoice = useVoiceStore((s) => s.recovery?.serverId === server)
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
  const [showRoomMembers, setShowRoomMembers] = useState(false)
  const [showChannels, setShowChannels] = useState(false)
  const channelsButton = useRef<HTMLButtonElement>(null)
  const channelTree = useRef<HTMLElement>(null)
  const destinationHeading = useRef<HTMLHeadingElement>(null)
  const focusDestination = useRef(false)
  const returnToRoom = useRef<string | null>(null)
  useLayoutEffect(() => {
    if (showChannels) {
      const tree = channelTree.current
      const target =
        tree?.querySelector<HTMLElement>('[aria-current="page"]') ??
        tree?.querySelector<HTMLElement>('.live-channel, button')
      target?.focus()
    } else if (focusDestination.current) destinationHeading.current?.focus({ preventScroll: true })
    else if (returnToRoom.current)
      document.getElementById(`lobby-preview-${returnToRoom.current}`)?.focus()
    focusDestination.current = false
    returnToRoom.current = null
  }, [showChannels, selected])
  function openChannel(channel: ChannelView, fromLobby = false) {
    focusDestination.current = fromLobby || !!window.matchMedia?.('(max-width: 900px)').matches
    setShowChannels(false)
    select(server, channel.id)
  }
  const [serverSettingsOpen, setServerSettingsOpen] = useState(false)
  const [rolesOpen, setRolesOpen] = useState(false)
  const [channelsOpen, setChannelsOpen] = useState(false)
  const [webhooksOpen, setWebhooksOpen] = useState(false)
  const [auditOpen, setAuditOpen] = useState(false)
  const [invitesOpen, setInvitesOpen] = useState(false)
  const [dmDialog, setDmDialog] = useState<DmMode | null>(null)
  const [dmWarning, setDmWarning] = useState('')
  useEffect(() => {
    void refreshVoice(server)
    return () => {
      if (useAppStore.getState().selectedServerId === server) return
      const state = useVoiceStore.getState()
      const previous = state.views[server]
      if (previous)
        state.select(
          server,
          workspaceDestination(
            previous,
            server,
            undefined,
            lastConversation(server, previous.ownUserId),
          ),
        )
    }
  }, [server])
  const members = useMemo(() => new Map(view?.members.map((m) => [m.id, m])), [view?.members])
  const participantsByUser = useMemo(
    () => new Map(view?.participants.map((participant) => [participant.userId, participant])),
    [view?.participants],
  )
  const participantsByRoom = useMemo(() => {
    const rooms = new Map<string, ParticipantView[]>()
    for (const participant of view?.participants ?? []) {
      const room = rooms.get(participant.channelId)
      if (room) room.push(participant)
      else rooms.set(participant.channelId, [participant])
    }
    return rooms
  }, [view?.participants])
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
  function openPersonalSettings(event: React.MouseEvent<HTMLButtonElement>) {
    event.currentTarget.focus()
    setAudioSettings(true)
  }
  const personalSettingsButton = (
    <button className="personal-settings-sidebar" onClick={openPersonalSettings}>
      <SlidersHorizontal size={16} /> Personal settings
    </button>
  )
  const audioDialog = audioSettings && <AudioSettings close={() => setAudioSettings(false)} />
  if (!view)
    return (
      <section className="live-workspace">
        {audioDialog}
        <aside className="live-channels" aria-label="Channel tree">
          {serverHeader}
          {personalSettingsButton}
        </aside>
        <main className="live-channel-main">
          <p role="status">Loading channels…</p>
          <button className="button secondary" onClick={() => void refreshVoice(server)}>
            Retry
          </button>
        </main>
      </section>
    )
  const title = currentDm ? dmName(currentDm, members, view.ownUserId) : current?.name
  const chosenMember = view.members.find((m) => m.id === menu)
  const voiceRoom = current?.kind === 3 ? current : undefined
  const rooms = view.channels.filter((c) => c.kind === 3)
  const voiceOnly = rooms.length > 0 && !view.channels.some((c) => c.kind === 2)
  const roster = groups.map((group) => (
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
          onMenu={setMenu}
        />
      ))}
    </section>
  ))
  return (
    <section
      className={`live-workspace ${chatChannel ? 'has-chat' : 'has-voice-room'}`}
      data-members-open={showRoomMembers || undefined}
      data-channels-open={showChannels || undefined}
    >
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
      {invitesOpen && summary && (view.canCreateInvites || view.canManageInvites) && (
        <InviteManager
          key={`${server}:${view.ownUserId}`}
          server={summary}
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
      <aside
        className="live-channels"
        aria-label="Channel tree"
        id="channel-tree"
        ref={channelTree}
        onKeyDown={(event) => {
          if (event.key === 'Escape' && showChannels && channelsButton.current?.offsetWidth) {
            event.preventDefault()
            setShowChannels(false)
            channelsButton.current.focus()
          }
        }}
      >
        {serverHeader}
        <div className="live-channel-scroll">
          <DmList
            server={server}
            view={view}
            selected={selected}
            connected={connected}
            select={(id) => {
              focusDestination.current = !!window.matchMedia?.('(max-width: 900px)').matches
              setShowChannels(false)
              select(server, id)
              setDmDialog(null)
              setDmWarning('')
            }}
            create={() => setDmDialog('create')}
          />
          {visibleChannels(view.channels, collapsed).map(({ channel, depth }) => {
            const participants = participantsByRoom.get(channel.id) ?? []
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
                    aria-label={`${channel.kind === 3 ? 'Preview voice room' : 'Open text channel'} ${channel.name}`}
                    aria-current={current?.id === channel.id ? 'page' : undefined}
                    aria-describedby={channel.kind === 3 ? `occupancy-${channel.id}` : undefined}
                    onClick={() => openChannel(channel)}
                  >
                    {channel.kind === 3 ? (
                      <Volume2 size={17} className="room-speaker" />
                    ) : (
                      <Hash size={17} />
                    )}{' '}
                    {channel.name}
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
                      <small id={`occupancy-${channel.id}`}>
                        {participants.length}
                        {channel.userLimit != null ? `/${channel.userLimit}` : ''}{' '}
                        <span className="sr-only">in voice</span>
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
                      onMenu={setMenu}
                    />
                  ) : null
                })}
              </div>
            )
          })}
        </div>
        {personalSettingsButton}
      </aside>
      <main className={`live-channel-main ${chatChannel ? 'text-workspace' : ''}`}>
        <header className={chatChannel ? 'room-toolbar conversation-toolbar' : 'room-toolbar'}>
          <button
            ref={channelsButton}
            className="button secondary room-channels-toggle"
            aria-label="Channels"
            title="Channels"
            aria-controls="channel-tree"
            aria-expanded={showChannels}
            onClick={() => setShowChannels(!showChannels)}
          >
            <PanelLeft size={18} />
          </button>

          {chatChannel ? (
            <div className="conversation-title">
              <small>{currentDm ? 'Direct messages' : label}</small>
              <h1 ref={destinationHeading} tabIndex={-1} title={title}>
                {currentDm ? (
                  <MessageCircle aria-hidden="true" size={20} />
                ) : (
                  <Hash aria-hidden="true" size={20} />
                )}
                <span>{title}</span>
              </h1>
            </div>
          ) : voiceRoom && voiceOnly ? (
            <button
              className="button secondary lobby-back"
              onClick={() => {
                returnToRoom.current = voiceRoom.id
                select(server, '')
              }}
            >
              <ArrowLeft size={16} /> Voice rooms
            </button>
          ) : voiceRoom ? (
            <span>
              <Volume2 size={20} /> Voice rooms
            </span>
          ) : (
            <span className="landing-context">
              <small>{label}</small>
              {rooms.length ? 'Voice rooms' : 'Channels'}
            </span>
          )}
          {(voiceRoom || chatChannel) && (
            <button
              className="button secondary"
              aria-label="Search messages"
              title="Search messages"
              disabled={!connected}
              onClick={(event) => {
                event.currentTarget.focus()
                setSearchOpen(true)
              }}
            >
              <Search size={16} />
              {chatChannel && 'Search'}
            </button>
          )}
          {chatChannel ? (
            <ChatMembers key={chatChannel.id}>
              {currentDm ? (
                <DmHeader
                  conversation={currentDm}
                  view={view}
                  connected={connected}
                  manage={setDmDialog}
                />
              ) : (
                roster
              )}
            </ChatMembers>
          ) : (
            <button
              className="button secondary"
              aria-expanded={showRoomMembers}
              aria-controls="server-roster"
              onClick={() => setShowRoomMembers(!showRoomMembers)}
            >
              <Users size={16} /> Members
            </button>
          )}
          <button
            className="button secondary personal-settings-compact"
            onClick={openPersonalSettings}
          >
            <SlidersHorizontal size={16} /> Personal settings
          </button>
        </header>
        {dmWarning && <p role="alert">{dmWarning}</p>}
        <ConnectionBanner
          reconnecting={status?.kind === 'reconnecting'}
          chat={!!chatChannel}
          serverName={label}
          recoveringVoice={recoveringVoice}
        />
        <ServerNotices server={server} />
        {voiceRoom ? (
          <VoiceRoomPreview
            headingRef={destinationHeading}
            server={server}
            serverName={label}
            view={view}
            channel={voiceRoom}
            connection={status?.kind}
            members={members}
            participants={participantsByRoom.get(voiceRoom.id) ?? []}
            openMember={setMenu}
            openAudio={() => setAudioSettings(true)}
          />
        ) : chatChannel ? (
          <Chat
            acceptingSends={connected || status?.kind === 'restoring'}
            key={`${server}:${view.ownUserId}:${chatChannel.id}`}
            server={server}
            channel={chatChannel.id}
            name={title ?? 'Conversation'}
            connected={connected}
          />
        ) : rooms.length ? (
          <VoiceLobby
            server={server}
            rooms={rooms}
            members={members}
            participants={participantsByRoom}
            connected={connected}
            preview={(room) => openChannel(room, true)}
          />
        ) : (
          <NoChannels server={server} />
        )}
        {voiceRoom && (
          <details className="voice-ui-settings">
            <summary>Keyboard shortcuts</summary>
            <p>
              Mute: {shortcutLabel('mute')} · Deafen: {shortcutLabel('deafen')} · Previous channel:{' '}
              {shortcutLabel('previous')} · Next channel: {shortcutLabel('next')}. These shortcuts
              work while the app is focused.
            </p>
            <p>
              Choose your devices and talking mode in Personal settings. Open Global shortcuts under
              More audio controls to set up push-to-talk outside the app.
            </p>
          </details>
        )}
      </main>
      {!chatChannel && (
        <aside
          className="live-members"
          id="server-roster"
          aria-label="Members"
          hidden={!showRoomMembers}
        >
          {roster}
        </aside>
      )}
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

function ConnectionBanner({
  reconnecting,
  chat,
  serverName,
  recoveringVoice,
}: {
  reconnecting: boolean
  chat: boolean
  serverName?: string
  recoveringVoice: boolean
}) {
  const [sawLoss, setSawLoss] = useState(reconnecting)
  if (reconnecting && !sawLoss) setSawLoss(true)
  useEffect(() => {
    if (!sawLoss || reconnecting) return
    const timer = window.setTimeout(() => setSawLoss(false), 5000)
    return () => window.clearTimeout(timer)
  }, [sawLoss, reconnecting])
  if (!sawLoss && !recoveringVoice) return null
  return (
    <p
      role="status"
      className={`connection-banner recovery-banner ${reconnecting ? 'lost' : 'restored'}`}
    >
      <strong>
        {reconnecting ? 'Connection lost' : chat ? 'Chat reconnected' : 'Server reconnected'}
      </strong>
      <span>
        {reconnecting
          ? `Reconnecting to ${serverName ?? 'the server'}. ${chat ? 'Your draft stays here.' : ''}`
          : recoveringVoice && chat
            ? 'Chat is available while voice recovers.'
            : `Connected to ${serverName ?? 'the server'}.`}
      </span>
    </p>
  )
}
