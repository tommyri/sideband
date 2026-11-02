import '@testing-library/jest-dom/vitest'
import { render, screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import App from './App'

describe('desktop shell', () => {
  it('renders the conversation and native event inspector', async () => {
    render(<App />)
    expect(screen.getByRole('heading', { name: 'Welcome to #general' })).toBeInTheDocument()
    expect(screen.getByText('Event inspector')).toBeInTheDocument()
    expect(await screen.findByLabelText('Product Studio')).toBeInTheDocument()
  })
})
