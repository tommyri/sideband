import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, beforeAll, beforeEach, expect, it, vi } from 'vitest'
import { ServerHome } from './ServerHome'
import { useAppStore } from '../../store/app-store'
import { subscribeToClientEvents } from '../../lib/events'
import { useServerNotices } from '../../lib/notices'
import type { AppEvent } from '../../lib/bindings'
import type { ClientEventPayload } from '../../lib/generated/ClientEventPayload'
import type { ServerSummary } from '../../lib/generated/ServerSummary'

const handlers = vi.hoisted(() => new Map<string, (event: { payload: unknown }) => void>())
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    handlers.set(name, handler)
    return () => handlers.delete(name)
  }),
}))
vi.mock('../../lib/tauri', () => ({
  isTauri: () => true,
  invokeBridge: vi.fn(() => new Promise(() => {})),
}))

const server = (id: string, status: ServerSummary['status']): ServerSummary => ({
  id,
  label: id,
  host: id,
  port: 5781,
  httpsPort: 5781,
  unread: 0,
  status,
})
function emit(serverId: string, event: ClientEventPayload) {
  const payload: AppEvent = { serverId, occurredAtMs: 0, event }
  act(() => handlers.get('ss://event')!({ payload }))
}
beforeAll(async () => {
  await subscribeToClientEvents()
})
beforeEach(() => {
  useServerNotices.setState({ notices: {} })
  useAppStore.setState({
    servers: [server('a', { kind: 'connected' }), server('b', { kind: 'connected' })],
    selectedServerId: 'a',
  })
})
afterEach(cleanup)

it('shows why the server removed this session, per server', () => {
  render(<ServerHome />)
  emit('a', { kind: 'removed', banned: true, reason: 'Spamming the lobby' })
  emit('a', { kind: 'connection', status: { kind: 'disconnected' } })
  expect(screen.getByRole('alert')).toHaveTextContent(
    'You were banned from this server: Spamming the lobby',
  )
  act(() => useAppStore.getState().selectServer('b'))
  expect(screen.queryByText(/banned/)).toBeNull()
  // Signing in again clears the removal.
  act(() => useAppStore.getState().selectServer('a'))
  emit('a', { kind: 'connection', status: { kind: 'connected' } })
  expect(screen.queryByText(/banned/)).toBeNull()
})

it('shows client notices such as a failed voice rejoin', () => {
  render(<ServerHome />)
  emit('a', { kind: 'notice', message: 'Could not rejoin voice: channel is full' })
  expect(screen.getByRole('status')).toHaveTextContent('Could not rejoin voice: channel is full')
  // A reconnect keeps it; dismissing removes it.
  emit('a', { kind: 'connection', status: { kind: 'connected' } })
  fireEvent.click(screen.getByRole('button', { name: 'Dismiss' }))
  expect(screen.queryByRole('status')).toBeNull()
})

it('shows unsolicited server errors with the wait the server asked for', () => {
  render(<ServerHome />)
  emit('a', { kind: 'serverError', code: 5, message: 'Slow down', retryAfterMs: 2500 })
  expect(screen.getByRole('status')).toHaveTextContent('Slow down. Try again in 3 s.')
})

it('says why a connection failed', () => {
  useAppStore.setState({
    servers: [server('a', { kind: 'failed', message: 'session expired' })],
  })
  render(<ServerHome />)
  expect(screen.getByRole('alert')).toHaveTextContent('session expired')
})
