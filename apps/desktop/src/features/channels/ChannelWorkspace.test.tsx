import '@testing-library/jest-dom/vitest'
import { Profiler } from 'react'
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ChannelWorkspace } from './ChannelWorkspace'
import { MemberRow } from './MemberRow'
import { VoiceDock } from '../voice/VoiceDock'
import { defaults, useAudioStore } from '../settings/audio/store'
import { refreshVoice, useVoiceStore } from '../voice/store'
import { useAppStore } from '../../store/app-store'
import { getServerView, joinVoice, leaveVoice, memberAction, voiceControl } from '../voice/api'
import { disconnect, loadDmPage } from '../../lib/api'
import { lastConversation, rememberConversation } from './destination'
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
  loadDmPage: vi.fn(),
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
const active = {
  serverId: 'server',
  channelId: 'voice',
  muted: false,
  deafened: false,
  ready: true,
}
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
  vi.stubGlobal(
    'ResizeObserver',
    class {
      observe() {}
      unobserve() {}
      disconnect() {}
    },
  )
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
    joining: null,
    error: '',
    deviceNotice: '',
  })
  vi.mocked(getServerView).mockResolvedValue(fixture())
  vi.mocked(joinVoice).mockResolvedValue()
  vi.mocked(memberAction).mockResolvedValue()
  vi.mocked(voiceControl).mockResolvedValue()
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})
describe('live channels and voice', () => {
  it('opens the chat roster on demand, returns focus, and shows only participants in a DM', async () => {
    const view = fixture()
    view.channels.push({ ...channel, kind: 2, id: 'text', name: 'general' })
    view.directMessages = [
      {
        id: 'dm',
        name: 'Crew',
        kind: 5,
        memberIds: ['own'],
        creatorId: 'own',
        lastActivity: 0,
        mentions: 0,
        unread: false,
      },
    ]
    vi.mocked(getServerView).mockResolvedValue(view)
    useVoiceStore.setState({ views: {}, selected: {} })
    render(<ChannelWorkspace server="server" />)
    await screen.findByRole('heading', { name: 'general' })
    const trigger = screen.getByRole('button', { name: 'Members' })
    expect(screen.queryByRole('dialog', { name: 'Members' })).toBeNull()
    fireEvent.click(trigger)
    const roster = screen.getByRole('dialog', { name: 'Members' })
    expect(within(roster).getByText('@real_account')).toBeInTheDocument()
    fireEvent(roster, new Event('cancel', { cancelable: true }))
    expect(trigger).toHaveFocus()
    expect(trigger).toHaveAttribute('aria-expanded', 'false')
    fireEvent.click(screen.getByRole('button', { name: 'Open group message Crew' }))
    const dmTrigger = screen.getByRole('button', { name: 'Members' })
    fireEvent.click(dmTrigger)
    const dm = within(screen.getByRole('dialog', { name: 'Members' }))
    expect(dm.getByText('@me')).toBeInTheDocument()
    expect(dm.queryByText('@real_account')).toBeNull()
    expect(dm.getByRole('button', { name: 'Rename' })).toBeInTheDocument()
    fireEvent.click(dm.getByRole('button', { name: 'Close members' }))
    expect(dmTrigger).toHaveFocus()
  })
  it('restores the last viewed chat on restart and rechecks access for each server and account', async () => {
    const general = { ...channel, kind: 2, id: 'general-id', name: 'General' }
    const news = { ...general, id: 'news-id', name: 'News' }
    const view = { ...fixture(), channels: [news, channel, general], activeVoice: null }
    useVoiceStore.setState({ selected: {}, views: {}, active: null })
    vi.mocked(getServerView).mockResolvedValue(view)
    const workspace = render(<ChannelWorkspace server="server" />)
    expect(await screen.findByRole('heading', { name: 'General' })).toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Open text channel News' }))
    fireEvent.click(screen.getByRole('button', { name: 'Preview voice room Studio' }))
    expect(lastConversation('server', 'own')?.id).toBe(news.id)
    workspace.unmount()
    useVoiceStore.setState({ selected: {}, views: {} })
    render(<ChannelWorkspace server="server" />)
    expect(await screen.findByRole('heading', { name: 'News' })).toBeInTheDocument()
    await act(async () => refreshVoice('other'))
    expect(useVoiceStore.getState().selected.other).toBe(general.id)

    vi.mocked(getServerView).mockResolvedValue({ ...view, ownUserId: 'another-user' })
    await act(async () => refreshVoice('server'))
    expect(screen.getByRole('heading', { name: 'General' })).toBeInTheDocument()
    vi.mocked(getServerView).mockResolvedValue({ ...view, channels: [channel, general] })
    await act(async () => refreshVoice('server'))
    expect(screen.queryByRole('button', { name: 'Open text channel News' })).toBeNull()
    expect(screen.getByRole('heading', { name: 'General' })).toBeInTheDocument()
    vi.mocked(getServerView).mockResolvedValue({ ...view, channels: [channel, news] })
    await act(async () => refreshVoice('server'))
    expect(screen.getByRole('heading', { name: 'News' })).toBeInTheDocument()
  })
  it('checks older DM pages for membership before restoring a saved DM', async () => {
    const dm = {
      id: 'old-dm',
      name: '',
      kind: 4,
      memberIds: ['own', 'remote'],
      creatorId: null,
      lastActivity: 0,
      mentions: 0,
      unread: false,
    }
    const view = { ...fixture(), moreDirectMessages: true, activeVoice: null }
    rememberConversation('server', 'own', { id: dm.id, kind: 'dm' })
    vi.mocked(getServerView)
      .mockResolvedValueOnce(view)
      .mockResolvedValueOnce({ ...view, moreDirectMessages: false, directMessages: [dm] })
    await refreshVoice('server')
    expect(loadDmPage).toHaveBeenCalledOnce()
    expect(useVoiceStore.getState().selected.server).toBe(dm.id)
    vi.mocked(getServerView)
      .mockResolvedValueOnce(view)
      .mockResolvedValueOnce({ ...view, moreDirectMessages: false })
    await refreshVoice('server')
    expect(useVoiceStore.getState().selected.server).toBe('')
    expect(useVoiceStore.getState().views.server!.directMessages).toEqual([])
  })
  it('honors a new DM selected before its membership event without mounting unverified history', async () => {
    const dm = {
      id: 'new',
      name: '',
      kind: 4,
      memberIds: ['own', 'remote'],
      creatorId: null,
      lastActivity: 0,
      mentions: 0,
      unread: false,
    }
    const view = { ...fixture(), activeVoice: null }
    vi.mocked(getServerView)
      .mockResolvedValueOnce(view)
      .mockResolvedValueOnce({ ...view, directMessages: [dm] })
    useVoiceStore.getState().select('server', dm.id)
    await refreshVoice('server')
    expect(useVoiceStore.getState().selected.server).toBe('')
    await refreshVoice('server')
    expect(useVoiceStore.getState().selected.server).toBe(dm.id)
  })
  it('previews occupied and empty rooms from the lobby and joins only on request', async () => {
    const view = { ...fixture(), activeVoice: null }
    view.channels.push({ ...channel, id: 'other', name: 'Backstage', canJoin: false })
    useVoiceStore.setState({ views: {}, active: null })
    vi.mocked(getServerView).mockResolvedValue(view)
    render(<ChannelWorkspace server="server" />)
    const lobby = within(await screen.findByRole('region', { name: 'Voice rooms' }))
    expect(lobby.getByRole('button', { name: 'Preview Studio' })).toHaveAccessibleDescription(
      'Same nickname',
    )
    expect(lobby.getByRole('button', { name: 'Preview Backstage' })).toHaveAccessibleDescription(
      /Room is empty/,
    )
    expect(lobby.getByRole('button', { name: 'Join voice in Backstage' })).toBeDisabled()
    fireEvent.click(lobby.getByRole('button', { name: 'Preview Studio' }))
    expect(screen.getByRole('heading', { name: 'Studio' })).toHaveFocus()
    fireEvent.click(screen.getByRole('button', { name: 'Voice rooms' }))
    expect(screen.getByRole('button', { name: 'Preview Studio' })).toHaveFocus()
    expect(joinVoice).not.toHaveBeenCalled()
    fireEvent.click(screen.getByRole('button', { name: 'Join voice in Studio' }))
    await waitFor(() => expect(joinVoice).toHaveBeenCalledExactlyOnceWith('server', 'voice'))
  })
  it('lets someone with no accessible rooms choose another saved server or add one', async () => {
    const view = {
      ...fixture(),
      channels: [],
      activeVoice: null,
      directMessages: [
        {
          id: 'unopened',
          name: 'Project group',
          kind: 5,
          memberIds: ['own', 'remote'],
          creatorId: 'remote',
          lastActivity: 1,
          unread: true,
          mentions: 0,
        },
      ],
    }
    vi.mocked(getServerView).mockResolvedValue(view)
    useVoiceStore.setState({ views: {}, active: null })
    useAppStore.setState((s) => ({
      servers: [...s.servers, { ...s.servers[0]!, id: 'work', label: 'Work' }],
    }))
    render(<ChannelWorkspace server="server" />)
    expect(
      await screen.findByRole('heading', { name: 'No channels available' }),
    ).toBeInTheDocument()
    expect(
      screen.getByRole('button', { name: 'Open group message Project group' }),
    ).not.toHaveAttribute('aria-current')
    fireEvent.click(screen.getByRole('button', { name: 'Switch server' }))
    fireEvent.click(screen.getByRole('menuitem', { name: 'Work' }))
    expect(useAppStore.getState().selectedServerId).toBe('work')
    act(() => useAppStore.setState((s) => ({ servers: s.servers.slice(0, 1) })))
    fireEvent.click(screen.getByRole('button', { name: 'Connect to another server' }))
    expect(useAppStore.getState().connectOpen).toBe(true)
  })
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
    ['ptt', /After joining, hold the talk button/, /Hold the talk button to speak/, 'Hold to talk'],
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
    expect(screen.queryByRole('menuitem', { name: /audio|personal settings/i })).toBeNull()
    const details = screen.getByRole('menuitem', { name: 'Server details' })
    fireEvent.keyDown(details, { key: 'End' })
    const end = screen.getByRole('menuitem', { name: 'Disconnect from server' })
    expect(end).toHaveFocus()
    fireEvent.keyDown(end, { key: 'ArrowUp' })
    expect(details).toHaveFocus()
    fireEvent.keyDown(details, { key: 'Escape' })
    expect(trigger).toHaveFocus()
    expect(screen.queryByRole('menu')).toBeNull()
  })
  it('renders immutable usernames and collapses category descendants', async () => {
    render(<ChannelWorkspace server="server" />)
    expect(screen.getAllByText('@real_account').length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: 'Preview voice room Studio' })).toBeEnabled()
    fireEvent.click(screen.getByRole('button', { name: /Social/ }))
    expect(
      screen.queryByRole('button', { name: 'Preview voice room Studio' }),
    ).not.toBeInTheDocument()
    await waitFor(() => expect(getServerView).toHaveBeenCalled())
  })
  it.each(['true', 'false'])(
    'previews without joining with the legacy double-click setting %s',
    async (setting) => {
      const view = { ...fixture(), activeVoice: null }
      localStorage.setItem('ss.doubleClickJoin', setting)
      vi.mocked(getServerView).mockResolvedValue(view)
      useVoiceStore.setState({ views: { server: view }, active: null })
      render(<ChannelWorkspace server="server" />)
      const row = screen.getByRole('button', { name: 'Preview voice room Studio' })
      fireEvent.click(row)
      fireEvent.doubleClick(row)
      const preview = within(screen.getByRole('region', { name: 'Studio' }))
      expect(preview.getByRole('region', { name: 'In the room' })).toHaveTextContent(
        '@real_account',
      )
      expect(preview.getByText('Push to talk')).toBeInTheDocument()
      expect(row).toHaveAccessibleDescription('1/30 in voice')
      expect(joinVoice).not.toHaveBeenCalled()
      expect(voiceControl).not.toHaveBeenCalled()
      expect(leaveVoice).not.toHaveBeenCalled()

      let finish!: () => void
      vi.mocked(joinVoice).mockReturnValueOnce(
        new Promise((resolve) => {
          finish = resolve
        }),
      )
      fireEvent.click(preview.getByRole('button', { name: 'Join voice' }))
      const joining = preview.getByRole('button', { name: 'Joining…' })
      expect(joining).toHaveAttribute('aria-disabled', 'true')
      fireEvent.click(joining)
      expect(joinVoice).toHaveBeenCalledTimes(1)
      expect(joinVoice).toHaveBeenCalledWith('server', 'voice')
      vi.mocked(getServerView).mockResolvedValue(fixture())
      await act(async () => finish())
      expect(preview.getByRole('status')).toHaveTextContent('You’re in Studio.')
    },
  )
  it('keeps the current call while previewing another room and allows retry after a failed join', async () => {
    const view = fixture()
    view.channels.push({ ...channel, id: 'other', name: 'Backstage' })
    vi.mocked(getServerView).mockResolvedValue(view)
    useVoiceStore.setState({ views: { server: view } })
    render(
      <>
        <ChannelWorkspace server="server" />
        <VoiceDock />
      </>,
    )
    const row = screen.getByRole('button', { name: 'Preview voice room Backstage' })
    fireEvent.click(row)
    const preview = within(screen.getByRole('region', { name: 'Backstage' }))
    expect(preview.getByText('No one is here yet.')).toBeInTheDocument()
    expect(preview.getByRole('status')).toHaveTextContent(
      'Joining this room will leave your current call.',
    )
    expect(screen.getByRole('region', { name: 'Voice controls' })).toHaveTextContent('Studio')
    expect(leaveVoice).not.toHaveBeenCalled()
    expect(joinVoice).not.toHaveBeenCalled()
    vi.mocked(joinVoice).mockRejectedValueOnce({ message: 'Room is full.' })
    fireEvent.click(preview.getByRole('button', { name: 'Join voice' }))
    expect(await screen.findByRole('alert')).toHaveTextContent(
      'Could not join Backstage. Room is full.',
    )
    await waitFor(() => expect(useVoiceStore.getState().busy).toBe(false))
    vi.mocked(getServerView).mockResolvedValue({
      ...view,
      activeVoice: { ...active, channelId: 'other' },
    })
    fireEvent.click(preview.getByRole('button', { name: 'Join voice' }))
    await waitFor(() =>
      expect(preview.getByRole('status')).toHaveTextContent('You’re in Backstage.'),
    )
    expect(screen.queryByRole('alert')).toBeNull()
  })
  it.each(['Space', 'Mouse0'])(
    'keeps room activation separate from the in-app %s talk binding',
    async (binding) => {
      useAudioStore.setState({ preferences: { ...defaults, binding } })
      const view = fixture()
      view.channels.push({ ...channel, id: 'backstage', name: 'Backstage' })
      vi.mocked(getServerView).mockResolvedValue(view)
      useVoiceStore.setState({ views: { server: view } })
      render(
        <>
          <ChannelWorkspace server="server" />
          <VoiceDock />
        </>,
      )
      await act(async () => {})
      vi.mocked(voiceControl).mockClear()
      const row = screen.getByRole('button', { name: 'Preview voice room Backstage' })
      if (binding === 'Space') {
        fireEvent.keyDown(row, { key: ' ', code: 'Space' })
        fireEvent.keyUp(row, { key: ' ', code: 'Space' })
      } else {
        fireEvent.pointerDown(row, { button: 0 })
        fireEvent.pointerUp(row, { button: 0 })
      }
      fireEvent.click(row)
      expect(screen.getByRole('region', { name: 'Backstage' })).toBeInTheDocument()
      expect(voiceControl).not.toHaveBeenCalled()
      expect(joinVoice).not.toHaveBeenCalled()
      expect(useVoiceStore.getState().active).toEqual(active)
    },
  )
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
  it('lets a member preview a visible room without permission to join', async () => {
    const view = { ...fixture(), activeVoice: null }
    view.channels[1] = { ...channel, canJoin: false }
    useVoiceStore.setState({ views: { server: view }, active: null })
    vi.mocked(getServerView).mockResolvedValue(view)
    render(<ChannelWorkspace server="server" />)
    fireEvent.click(screen.getByRole('button', { name: 'Preview voice room Studio' }))
    expect(screen.getByRole('button', { name: 'Join voice' })).toBeDisabled()
    expect(screen.getByRole('status')).toHaveTextContent('permission to join')
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
