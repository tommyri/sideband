import '@testing-library/jest-dom/vitest'
import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import App from './App'
import { useAppStore } from './store/app-store'

afterEach(() => {
  cleanup()
  vi.unstubAllEnvs()
  useAppStore.setState({ debugOpen: false })
})

describe('desktop shell', () => {
  it('opens the event inspector only on request in development', async () => {
    render(<App />)
    expect(screen.getByRole('heading', { name: 'Welcome to #general' })).toBeInTheDocument()
    expect(await screen.findByLabelText('Product Studio')).toHaveTextContent('PS')
    expect(screen.queryByText('Event inspector')).not.toBeInTheDocument()
    fireEvent.click(screen.getByRole('button', { name: 'Toggle event inspector' }))
    expect(screen.getByText('Event inspector')).toBeInTheDocument()
    expect(screen.getByText('Browser preview')).toBeInTheDocument()
  })

  it('packaged builds show no event inspector and no placeholder identity', async () => {
    vi.stubEnv('DEV', false)
    render(<App />)
    expect(await screen.findByLabelText('Product Studio')).toHaveTextContent('PS')
    expect(screen.queryByRole('button', { name: 'Toggle event inspector' })).toBeNull()
    act(() => useAppStore.setState({ debugOpen: true }))
    expect(screen.queryByText('Event inspector')).toBeNull()
    expect(screen.queryByText('TR')).toBeNull()
    expect(screen.queryByText('▦')).toBeNull()
    expect(screen.queryByRole('img', { name: /Signed in as/ })).toBeNull()
  })
})
