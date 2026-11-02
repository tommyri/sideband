import { useCallback, useEffect, useMemo, useState } from 'react'
import { useAppStore } from '../../store/app-store'
import type { ChannelView } from '../../lib/generated/ChannelView'
import { refreshVoice, runVoice, useVoiceStore } from '../voice/store'
import { joinVoice, leaveVoice, memberAction } from '../voice/api'
import { MemberRow } from './MemberRow'
import { MemberMenu } from './MemberMenu'
import { ServerHome } from '../onboarding/ServerHome'

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
  const view = useVoiceStore((s) => s.views[server])
  const selected = useVoiceStore((s) => s.selected[server])
  const select = useVoiceStore((s) => s.select)
  const busy = useVoiceStore((s) => s.busy)
  const active = useVoiceStore((s) => s.active)
  const status = useAppStore((s) => s.servers.find((item) => item.id === server)?.status)
  const label = useAppStore((s) => s.servers.find((item) => item.id === server)?.label)
  const [collapsed, setCollapsed] = useState(new Set<string>())
  const [menu, setMenu] = useState<string | null>(null)
  const [details, setDetails] = useState(false)
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
        <button className="button secondary" onClick={() => setDetails(false)}>
          Back to channels
        </button>
        <ServerHome />
      </section>
    )
  if (!view)
    return (
      <main className="server-home">
        <p role="status">Loading channels…</p>
        <button className="button secondary" onClick={() => void refreshVoice(server)}>
          Retry
        </button>
      </main>
    )
  const current = view.channels.find(
    (c) => c.id === (selected ?? (active?.serverId === server ? active.channelId : undefined)),
  )
  const chosenMember = view.members.find((m) => m.id === menu)
  const connected = status?.kind === 'connected'
  const join = (channel: ChannelView) => {
    select(server, channel.id)
    if (channel.kind === 3) void runVoice(() => joinVoice(server, channel.id))
  }
  return (
    <section className="live-workspace">
      <aside className="live-channels" aria-label="Channel tree">
        <header>
          <h2>{label}</h2>
          <button className="button secondary" onClick={() => setDetails(true)}>
            Server details
          </button>
        </header>
        <div className="live-channel-scroll">
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
      <main className="live-channel-main">
        <header>
          <p className="eyebrow">{current?.kind === 3 ? 'Voice channel' : 'Your community'}</p>
          <h1>{current?.name ?? 'Choose a channel'}</h1>
          {current?.kind === 3 && (
            <p>
              {current.profile} profile ·{' '}
              {view.participants.filter((p) => p.channelId === current.id).length} in voice
            </p>
          )}
        </header>
        {status?.kind === 'reconnecting' && (
          <p role="status" className="connection-banner">
            Reconnecting to {label} (attempt {status.attempt})… Your microphone gate is closed.
          </p>
        )}
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
            <p>
              Use Hold to talk in the call controls. Your microphone only transmits while held; mute
              and deafen also close transmission.
            </p>
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
        ) : (
          <p>
            {current
              ? 'Text messaging is coming in the next test build.'
              : 'Select a voice channel to see its members and join the conversation.'}
          </p>
        )}
        <details className="voice-ui-settings">
          <summary>Interaction settings</summary>
          <label>
            <input
              type="checkbox"
              defaultChecked={localStorage.getItem('ss.doubleClickJoin') === 'true'}
              onChange={(e) => localStorage.setItem('ss.doubleClickJoin', String(e.target.checked))}
            />{' '}
            Double-click voice channels to join
          </label>
          <p>
            Ctrl/Cmd + Shift + M: mute · D: deafen · ↑/↓: select channel. Shortcuts work while this
            app is focused.
          </p>
          <p>
            Microphone/output selection and global push-to-talk settings are coming in a future test
            build. This build uses system-default devices.
          </p>
        </details>
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
