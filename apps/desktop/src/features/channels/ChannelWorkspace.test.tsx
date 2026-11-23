import '@testing-library/jest-dom/vitest'
import { Profiler } from 'react'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ChannelWorkspace } from './ChannelWorkspace'
import { MemberRow } from './MemberRow'
import { VoiceDock } from '../voice/VoiceDock'
import { defaults, useAudioStore } from '../settings/audio/store'
import { useVoiceStore } from '../voice/store'
import { useAppStore } from '../../store/app-store'
import { getServerView, joinVoice, leaveVoice, memberAction, voiceControl } from '../voice/api'
import { disconnect } from '../../lib/api'
import type { ServerView } from '../../lib/generated/ServerView'

vi.mock('../voice/api', () => ({
  getServerView: vi.fn(),
  joinVoice: vi.fn(),
  leaveVoice: vi.fn(),
  memberAction: vi.fn(),
  voiceControl: vi.fn(),
}))
vi.mock('../../lib/api', () => ({
  getSnapshot: vi.fn(async () => ({
    servers: [
      {
        id: 'server',
        label: 'Friends',
        host: 'localhost',
        port: 5781,
        httpsPort: 5781,
        unread: 0,
        status: { kind: 'connected' },
      },
    ],
  })),
  disconnect: vi.fn(),
}))
const member = {
  id: 'remote',
  username: 'real_account',
  displayName: 'Same nickname',
  roleName: 'Members',
  roleColor: null,
  rolePosition: 0,
  presence: 'online',
  activity: '',
  canKick: false,
  canBan: false,
  canMute: false,
  canMove: false,
  volume: 100,
  localMuted: false,
  bot: false,
}
const channel = {
  id: 'voice',
  parentId: 'category',
  name: 'Studio',
  kind: 3,
  profile: 'voice',
  userLimit: 30,
  canJoin: true,
  canSpeak: true,
  unread: false,
  mentions: 0,
}
const active = { serverId: 'server', channelId: 'voice', muted: false, deafened: false }
const fixture = (): ServerView => ({
  ownUserId: 'own',
  canModerateMembers: false,
  canManageServer: false,
  canManageRoles: false,
  canManageChannels: false,
  canManageWebhooks: false,
  canViewAuditLog: false,
  canCreateInvites: false,
  canManageInvites: false,
  directMessages: [],
  moreDirectMessages: false,
  channels: [{ ...channel, id: 'category', parentId: null, kind: 1, name: 'Social' }, channel],
  members: [{ ...member, id: 'own', username: 'me' }, member],
  participants: [
    {
      userId: 'remote',
      connectionId: 'conn',
      channelId: 'voice',
      muted: false,
      deafened: false,
      serverMuted: false,
    },
  ],
  activeVoice: active,
})
beforeEach(() => {
  vi.resetAllMocks()
  localStorage.clear()
  useAudioStore.setState({ preferences: defaults, loaded: true })
  HTMLDialogElement.prototype.showModal = function () {
    this.setAttribute('open', '')
  }
  HTMLDialogElement.prototype.close = function () {
    this.removeAttribute('open')
  }
  useAppStore.setState({
    servers: [
      {
        id: 'server',
        label: 'Friends',
        host: 'localhost',
        port: 5781,
        httpsPort: 5781,
        unread: 0,
        status: { kind: 'connected' },
      },
    ],
    selectedServerId: 'server',
  })
  useVoiceStore.setState({
    views: { server: fixture() },
    active,
    speaking: {},
    stats: {},
    selected: {},
    busy: false,
    error: '',
    deviceNotice: '',
  })
  vi.mocked(getServerView).mockResolvedValue(fixture())
  vi.mocked(joinVoice).mockResolvedValue()
  vi.mocked(memberAction).mockResolvedValue()
  vi.mocked(voiceControl).mockResolvedValue()
})
afterEach(cleanup)
describe('live channels and voice', () => {
  it('releases the dock row when idle and keeps failed joins dismissible', () => {
    const { container } = render(<VoiceDock />)
    expect(screen.getByRole('region', { name: 'Voice controls' })).toBeInTheDocument()
    act(() => useVoiceStore.setState({ active: null }))
    expect(container).toBeEmptyDOMElement()
    act(() => useVoiceStore.setState({ error: 'Could not join voice.' }))
    expect(screen.getByRole('alert')).toHaveTextContent('Could not join voice.')
    fireEvent.click(screen.getByRole('button', { name: 'Dismiss voice notice' }))
    expect(container).toBeEmptyDOMElement()
  })
  it.each([
    ['ptt', /After joining, hold the talk button/, /Hold the talk button or/, 'Hold to talk'],
    [
      'vad',
      /After joining, your microphone will transmit when it detects speech/,
      /listening for speech/,
      'Voice activation',
    ],
    [
      'open',
      /transmit automatically as soon as you join/,
      /Open microphone is on/,
      'Open microphone',
    ],
  ] as const)(
    'shows the selected %s mode before joining and follows the native transmit indicator in the call',
    async (mode, preview, inCall, label) => {
      const view = { ...fixture(), activeVoice: null }
      vi.mocked(getServerView).mockResolvedValue(view)
      useVoiceStore.setState({
        active: null,
        views: { server: view },
        selected: { server: 'voice' },
      })
      useAudioStore.setState({ preferences: { ...defaults, mode } })
      render(
        <>
          <ChannelWorkspace server="server" />
          <VoiceDock />
        </>,
      )
      await act(async () => {})
      expect(screen.getByText(preview)).toBeInTheDocument()
      act(() => useVoiceStore.setState({ active }))
      expect(screen.getByText(inCall)).toBeInTheDocument()
      expect(screen.getByRole('button', { name: label })).toBeInTheDocument()
      act(() => useVoiceStore.setState({ speaking: { server: ['own'] } }))
      expect(
        screen.getByText('Your microphone is transmitting to this channel.'),
      ).toBeInTheDocument()
      expect(screen.getByRole('button', { name: 'Transmitting' })).toBeInTheDocument()
      act(() => useVoiceStore.setState({ speaking: { server: ['remote'] } }))
      expect(screen.getByText(inCall)).toBeInTheDocument()
    },
  )
  it('gives the reason transmission is blocked even if the last speaking event said otherwise', async () => {
    render(
      <>
        <ChannelWorkspace server="server" />
        <VoiceDock />
      </>,
    )
    await act(async () => {})
    act(() => {
      useAudioStore.setState({ preferences: { ...defaults, mode: 'open' } })
      useVoiceStore.setState({ speaking: { server: ['own'] }, active: { ...active, muted: true } })
    })
    expect(screen.getByRole('button', { name: 'Muted' })).toBeDisabled()
    expect(screen.getByText('You are muted. Others cannot hear you.')).toBeInTheDocument()
    act(() => useVoiceStore.setState({ active: { ...active, muted: true, deafened: true } }))
    expect(screen.getByRole('button', { name: 'Deafened' })).toBeDisabled()
    const view = fixture()
    view.participants = [{ ...view.participants[0]!, userId: 'own', serverMuted: true }]
    act(() => useVoiceStore.setState({ active, views: { server: view } }))
    expect(screen.getByText('A moderator muted you in this channel.')).toBeInTheDocument()
    act(() =>
      useVoiceStore.setState({
        views: {
          server: {
            ...view,
            participants: [],
            channels: [{ ...channel, canSpeak: false }],
          },
        },
      }),
    )
    expect(screen.getByRole('button', { name: 'Listen only' })).toBeDisabled()
    act(() =>
      useAppStore.setState((s) => ({
        servers: s.servers.map((item) => ({
          ...item,
          status: { kind: 'reconnecting', attempt: 1, delayMs: 2000 },
        })),
      })),
    )
    expect(
      screen.getByText('Voice transmission is paused while this call reconnects.'),
    ).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Transmitting' })).toBeNull()
  })
  it('scopes the mic status to the selected channel and never assumes PTT before settings load', async () => {
    const other = { ...channel, id: 'other', name: 'Other' }
    const view = fixture()
    view.channels.push(other)
    vi.mocked(getServerView).mockResolvedValue(view)
    useVoiceStore.setState({
      views: { server: view },
      selected: { server: 'other' },
      speaking: { server: ['own'] },
    })
    useAudioStore.setState({ loaded: false })
    render(<ChannelWorkspace server="server" />)
    expect(screen.getByText('Check your talking mode in Audio settings.')).toBeInTheDocument()
    act(() => useAudioStore.setState({ loaded: true, preferences: { ...defaults, mode: 'open' } }))
    expect(
      await screen.findByText(/transmit automatically as soon as you join/),
    ).toBeInTheDocument()
  })
  it('keeps Leave voice in the call dock and disconnects the server whose menu was opened', async () => {
    const view = { ...fixture(), activeVoice: { ...active, serverId: 'other' } }
    useVoiceStore.setState({ views: { server: view }, active: view.activeVoice })
    vi.mocked(getServerView).mockResolvedValue(view)
    render(
      <>
        <ChannelWorkspace server="server" />
        <VoiceDock />
      </>,
    )
    const dock = within(screen.getByRole('region', { name: 'Voice controls' }))
    expect(dock.queryByRole('button', { name: /Disconnect/ })).toBeNull()
    fireEvent.click(dock.getByRole('button', { name: 'Leave voice' }))
    await waitFor(() => expect(useVoiceStore.getState().busy).toBe(false))
    expect(leaveVoice).toHaveBeenCalledOnce()
    expect(disconnect).not.toHaveBeenCalled()
    const trigger = screen.getByRole('button', { name: 'Server menu for Friends' })
    fireEvent.keyDown(trigger, { key: 'ArrowUp' })
    expect(screen.queryByRole('menuitem', { name: 'Server settings' })).toBeNull()
    const end = screen.getByRole('menuitem', { name: 'Disconnect from server' })
    expect(end).toHaveFocus()
    fireEvent.click(end)
    await waitFor(() => expect(disconnect).toHaveBeenCalledWith('server'))
    expect(screen.queryByRole('menu')).toBeNull()
  })
  it('shows permitted administration in the menu and skips unavailable actions during reconnection', async () => {
    const view = { ...fixture(), canManageChannels: true, canViewAuditLog: true, activeVoice: null }
    useVoiceStore.setState({ views: { server: view }, active: null })
    vi.mocked(getServerView).mockResolvedValue(view)
    render(<ChannelWorkspace server="server" />)
    await act(async () => {})
    const trigger = screen.getByRole('button', { name: 'Server menu for Friends' })
    expect(screen.queryByText('Manage channels')).toBeNull()
    fireEvent.click(trigger)
    expect(screen.getByRole('menuitem', { name: 'Manage channels' })).toBeEnabled()
    expect(screen.getByRole('menuitem', { name: 'Roles and permissions' })).toBeEnabled()
    expect(screen.queryByRole('menuitem', { name: 'Invites' })).toBeNull()
    act(() =>
      useAppStore.setState((s) => ({
        servers: s.servers.map((item) => ({
          ...item,
          status: { kind: 'reconnecting', attempt: 1, delayMs: 2000 },
        })),
      })),
    )
    expect(screen.getByRole('menuitem', { name: 'Audit log' })).toBeDisabled()
    const audio = screen.getByRole('menuitem', { name: 'Audio settings' })
    fireEvent.keyDown(audio, { key: 'End' })
    const end = screen.getByRole('menuitem', { name: 'Disconnect from server' })
    expect(end).toHaveFocus()
    fireEvent.keyDown(end, { key: 'ArrowUp' })
    expect(audio).toHaveFocus()
    fireEvent.keyDown(audio, { key: 'Escape' })
    expect(trigger).toHaveFocus()
    expect(screen.queryByRole('menu')).toBeNull()
  })
  it('renders immutable usernames and collapses category descendants', async () => {
    render(<ChannelWorkspace server="server" />)
    expect(screen.getAllByText('@real_account').length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: 'Join voice channel Studio' })).toBeEnabled()
    fireEvent.click(screen.getByRole('button', { name: /Social/ }))
    expect(
      screen.queryByRole('button', { name: 'Join voice channel Studio' }),
    ).not.toBeInTheDocument()
    await waitFor(() => expect(getServerView).toHaveBeenCalled())
  })
  it('joins on click and honors the double-click preference', async () => {
    render(<ChannelWorkspace server="server" />)
    fireEvent.click(screen.getByRole('button', { name: 'Join voice channel Studio' }))
    await waitFor(() => expect(joinVoice).toHaveBeenCalledWith('server', 'voice'))
    await waitFor(() => expect(useVoiceStore.getState().busy).toBe(false))
    vi.mocked(joinVoice).mockClear()
    localStorage.setItem('ss.doubleClickJoin', 'true')
    fireEvent.click(screen.getByRole('button', { name: 'Join voice channel Studio' }))
    expect(joinVoice).not.toHaveBeenCalled()
    fireEvent.doubleClick(screen.getByRole('button', { name: 'Join voice channel Studio' }))
    await waitFor(() => expect(joinVoice).toHaveBeenCalledTimes(1))
  })
  it('disables denied moderation but allows local playback controls', async () => {
    render(<ChannelWorkspace server="server" />)
    fireEvent.click(screen.getAllByRole('button', { name: /@real_account/ })[0]!)
    expect(screen.getByRole('button', { name: 'Server mute' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Kick…' })).toBeDisabled()
    expect(screen.getByRole('button', { name: 'Ban…' })).toBeDisabled()
    fireEvent.change(screen.getByRole('slider', { name: 'Playback volume' }), {
      target: { value: '150' },
    })
    fireEvent.click(screen.getByRole('button', { name: 'Apply volume' }))
    await waitFor(() =>
      expect(memberAction).toHaveBeenCalledWith(
        expect.objectContaining({ userId: 'remote', action: 'volume', volume: 150 }),
      ),
    )
  })
  it('opens and releases in-app PTT on keyboard and focus loss', async () => {
    render(<VoiceDock />)
    const talk = screen.getByRole('button', { name: 'Hold to talk' })
    fireEvent.keyDown(talk, { key: ' ' })
    expect(voiceControl).toHaveBeenLastCalledWith('ptt', true)
    fireEvent.blur(window)
    expect(voiceControl).toHaveBeenLastCalledWith('ptt', false)
    fireEvent.keyDown(window, { key: 'm', ctrlKey: true, shiftKey: true })
    await waitFor(() => expect(voiceControl).toHaveBeenCalledWith('mute', true))
  })
  it('reserves Ctrl+Shift+D for diagnostics and requires Alt for built-in deafen', async () => {
    render(<VoiceDock />)
    fireEvent.keyDown(window, { key: 'D', code: 'KeyD', ctrlKey: true, shiftKey: true })
    expect(voiceControl).not.toHaveBeenCalledWith('deafen', true)
    fireEvent.keyDown(window, {
      key: 'Î',
      code: 'KeyD',
      ctrlKey: true,
      shiftKey: true,
      altKey: true,
    })
    await waitFor(() => expect(voiceControl).toHaveBeenCalledWith('deafen', true))
  })
  it('does not join a channel when permissions are absent', async () => {
    const view = fixture()
    view.channels[1] = { ...channel, canJoin: false }
    useVoiceStore.setState({ views: { server: view } })
    vi.mocked(getServerView).mockResolvedValue(view)
    render(<ChannelWorkspace server="server" />)
    expect(screen.getByRole('button', { name: 'Join voice channel Studio' })).toBeDisabled()
    await waitFor(() => expect(getServerView).toHaveBeenCalled())
    expect(joinVoice).not.toHaveBeenCalled()
  })
  it('tags bot accounts and only them', () => {
    const menu = vi.fn()
    render(
      <>
        <MemberRow server="server" member={{ ...member, id: 'bot', bot: true }} onMenu={menu} />
        <MemberRow server="server" member={{ ...member, id: 'person' }} onMenu={menu} />
      </>,
    )
    expect(screen.getAllByTitle('Bot account')).toHaveLength(1)
  })
  it('updates only the speaking member row, not unrelated rows', () => {
    const changed = vi.fn()
    const unchanged = vi.fn()
    const menu = vi.fn()
    render(
      <>
        <Profiler id="changed" onRender={changed}>
          <MemberRow server="server" member={member} onMenu={menu} />
        </Profiler>
        <Profiler id="unchanged" onRender={unchanged}>
          <MemberRow server="server" member={{ ...member, id: 'other' }} onMenu={menu} />
        </Profiler>
      </>,
    )
    changed.mockClear()
    unchanged.mockClear()
    act(() => useVoiceStore.setState({ speaking: { server: ['remote'] } }))
    expect(changed).toHaveBeenCalledTimes(1)
    expect(unchanged).not.toHaveBeenCalled()
    expect(screen.getByLabelText('Speaking')).toBeInTheDocument()
  })
  it('renders 50 channels with 30 members each and updates speaking without rebuilding the tree', async () => {
    const view = fixture()
    view.channels = Array.from({ length: 50 }, (_, i) => ({
      ...channel,
      id: `channel-${i}`,
      parentId: null,
      name: `Room ${i}`,
    }))
    view.members = Array.from({ length: 1500 }, (_, i) => ({
      ...member,
      id: `user-${i}`,
      username: `user_${i}`,
    }))
    view.participants = view.members.map((m, i) => ({
      userId: m.id,
      connectionId: `conn-${i}`,
      channelId: `channel-${Math.floor(i / 30)}`,
      muted: false,
      deafened: false,
      serverMuted: false,
    }))
    view.activeVoice = null
    useVoiceStore.setState({ views: { server: view }, active: null })
    vi.mocked(getServerView).mockResolvedValue(view)
    const profile = vi.fn()
    const rendered = render(
      <Profiler id="large-channel-tree" onRender={profile}>
        <ChannelWorkspace server="server" />
      </Profiler>,
    )
    await waitFor(() => expect(getServerView).toHaveBeenCalled())
    expect(rendered.container.querySelectorAll('.live-channel')).toHaveLength(50)
    profile.mockClear()
    act(() => useVoiceStore.setState({ speaking: { server: ['user-0'] } }))
    expect(rendered.container.querySelectorAll('.live-avatar.speaking')).toHaveLength(2)
    expect(profile).toHaveBeenCalledTimes(1)
  }, 20000)
})
