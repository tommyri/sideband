import { useState, type FormEvent } from 'react'
import { Radio, X } from 'lucide-react'
import { connect } from '../lib/api'
import type { ApiError, ConnectInput } from '../lib/bindings'
import { useAppStore } from '../store/app-store'

const initialInput: ConnectInput = {
  host: '127.0.0.1',
  port: 5781,
  httpsPort: 5781,
  username: 'local',
  password: '',
}

export function ConnectDialog() {
  const open = useAppStore((state) => state.connectOpen)
  const setOpen = useAppStore((state) => state.setConnectOpen)
  const addConnection = useAppStore((state) => state.addConnection)
  const [input, setInput] = useState(initialInput)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState(false)

  if (!open) return null

  async function submit(event: FormEvent) {
    event.preventDefault()
    setPending(true)
    setError(null)
    try {
      addConnection(await connect(input))
    } catch (caught) {
      const apiError = caught as Partial<ApiError>
      setError(apiError.message ?? (caught instanceof Error ? caught.message : 'Connection failed'))
    } finally {
      setPending(false)
    }
  }

  return (
    <div className="dialog-backdrop" role="presentation" onMouseDown={() => setOpen(false)}>
      <section
        className="connect-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="connect-title"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header>
          <div className="dialog-mark" aria-hidden="true">
            <Radio size={18} />
          </div>
          <div>
            <p className="eyebrow">Development connection</p>
            <h2 id="connect-title">Connect to a server</h2>
          </div>
          <button
            className="icon-button dialog-close"
            onClick={() => setOpen(false)}
            aria-label="Close"
          >
            <X size={18} />
          </button>
        </header>
        <form onSubmit={submit}>
          <label>
            Host
            <input
              value={input.host}
              onChange={(event) => setInput({ ...input, host: event.target.value })}
              autoFocus
              required
            />
          </label>
          <div className="field-row">
            <label>
              QUIC port
              <input
                type="number"
                min="1"
                max="65535"
                value={input.port}
                onChange={(event) => setInput({ ...input, port: event.target.valueAsNumber })}
                required
              />
            </label>
            <label>
              HTTPS port
              <input
                type="number"
                min="1"
                max="65535"
                value={input.httpsPort}
                onChange={(event) => setInput({ ...input, httpsPort: event.target.valueAsNumber })}
                required
              />
            </label>
          </div>
          <label>
            Username
            <input
              value={input.username}
              onChange={(event) => setInput({ ...input, username: event.target.value })}
              required
            />
          </label>
          <label>
            Password
            <input
              type="password"
              value={input.password}
              onChange={(event) => setInput({ ...input, password: event.target.value })}
              required
            />
          </label>
          {error && <p className="form-error">{error}</p>}
          <div className="dialog-actions">
            <button type="button" className="button secondary" onClick={() => setOpen(false)}>
              Cancel
            </button>
            <button type="submit" className="button primary" disabled={pending}>
              {pending ? 'Connecting…' : 'Connect securely'}
            </button>
          </div>
        </form>
      </section>
    </div>
  )
}
