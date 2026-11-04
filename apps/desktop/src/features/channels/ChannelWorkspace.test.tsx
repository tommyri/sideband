import '@testing-library/jest-dom/vitest'
import { Profiler } from 'react'
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { ChannelWorkspace } from './ChannelWorkspace'
import { MemberRow } from './MemberRow'
import { VoiceDock } from '../voice/VoiceDock'
import { useVoiceStore } from '../voice/store'
import { useAppStore } from '../../store/app-store'
import { getServerView, joinVoice, memberAction, voiceControl } from '../voice/api'
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
  directMessages: [],
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
