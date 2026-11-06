import { useEffect, useRef, useState, type FormEvent } from 'react'
import { Radio, X } from 'lucide-react'
import {
  approveFingerprint,
  connect,
  getSnapshot,
  probeServer,
  enablePublicCertificate,
} from '../../lib/api'
import type { ApiError, ProbeInput, ServerProbe, ServerSummary } from '../../lib/bindings'
import { useAppStore } from '../../store/app-store'
import { authError, friendlyError, groupedFingerprint, parseAddress } from './validation'

export function ConnectDialog() {
  const open = useAppStore((s) => s.connectOpen)
  const server = useAppStore((s) => s.connectServer)
  return open ? <Onboarding server={server} /> : null
}

function Onboarding({ server }: { server: ServerSummary | null }) {
  const close = useAppStore((s) => s.setConnectOpen)
  const add = useAppStore((s) => s.addConnection)
  const dialog = useRef<HTMLDialogElement>(null)
  const [address, setAddress] = useState(
    server ? `${server.host.includes(':') ? `[${server.host}]` : server.host}:${server.port}` : '',
  )
  const [httpsPort, setHttpsPort] = useState(
    server && server.httpsPort !== server.port ? String(server.httpsPort) : '',
  )
  const [probe, setProbe] = useState<ServerProbe | null>(null)
  const [target, setTarget] = useState<ProbeInput | null>(null)
  const [step, setStep] = useState<'address' | 'trust' | 'auth'>('address')
  const [checking, setChecking] = useState(false)
  const [retry, setRetry] = useState(0)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const [mode, setMode] = useState<'login' | 'register'>('login')
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [invite, setInvite] = useState('')
  const mounted = useRef(true)

  useEffect(() => {
    mounted.current = true
    const previous = document.activeElement as HTMLElement | null
    const modal = dialog.current
    modal?.showModal()
    return () => {
      mounted.current = false
      modal?.close()
      previous?.focus()
    }
  }, [])

  useEffect(() => {
    const field = dialog.current?.querySelector<HTMLElement>(step === 'trust' ? 'h2' : 'input')
    field?.focus()
  }, [step])

  useEffect(() => {
    if (step !== 'address' || pending) return
    let active = true
    const timer = window.setTimeout(async () => {
      if (!address.trim()) return
      let input: ProbeInput
      try {
        input = parseAddress(address, httpsPort)
      } catch (e) {
        setError(friendlyError(e))
        return
      }
      setChecking(true)
      try {
        const result = await probeServer(input)
        if (active) {
          setProbe(result)
          setTarget(input)
          setError('')
        }
      } catch (e) {
        if (active) setError(friendlyError(e))
      } finally {
        if (active) setChecking(false)
      }
    }, 600)
    return () => {
      active = false
      window.clearTimeout(timer)
    }
  }, [address, httpsPort, step, retry, pending])

  function editAddress(value: string, https = false) {
    setProbe(null)
    setTarget(null)
    setChecking(false)
    setError('')
    if (https) setHttpsPort(value)
    else setAddress(value)
  }

  async function authenticate(saved = false) {
    if (!target) return
    const invalid = saved ? null : authError(username, password, mode === 'register')
    if (invalid) {
      setError(invalid)
      return
    }
    if (!saved && mode === 'register' && probe?.registrationMode === 'invite' && !invite.trim()) {
      setError('Enter the invite code from the server owner.')
      return
    }
    setPending(true)
    setError('')
    try {
      const result = await connect({
        ...target,
        username: username.trim(),
        password,
        mode: saved ? 'saved' : mode,
        inviteCode: invite.trim(),
      })
      if (mounted.current) {
        setPassword('')
        add(result, target)
        void getSnapshot()
          .then((snapshot) => useAppStore.getState().hydrate(snapshot.servers))
          .catch(() => undefined)
      }
    } catch (e) {
      if (!mounted.current) return
      const failure = e as Partial<ApiError>
      setPassword('')
      if (failure.fingerprint && probe) {
        setProbe({
          ...probe,
          trusted: false,
          previousFingerprint: probe.fingerprint,
          fingerprint: failure.fingerprint,
        })
        setStep('trust')
      } else {
        setError(friendlyError(e))
        setStep('auth')
      }
    } finally {
      if (mounted.current) setPending(false)
    }
  }

  async function switchDomainTrust() {
    setPending(true)
    setError('')
    try {
      const input = parseAddress(address, httpsPort)
      const result = await enablePublicCertificate(input)
      if (mounted.current) {
        setTarget(input)
        setProbe(result)
        setStep('auth')
      }
    } catch (e) {
      if (mounted.current) setError(friendlyError(e))
    } finally {
      if (mounted.current) setPending(false)
    }
  }

  async function trust() {
    if (!probe || !target) return
    setPending(true)
    setError('')
    try {
      await approveFingerprint({
        host: target.host,
        port: target.port,
        fingerprint: probe.fingerprint,
      })
      if (mounted.current) setStep('auth')
    } catch (e) {
      if (mounted.current) setError(friendlyError(e))
    } finally {
      if (mounted.current) setPending(false)
    }
  }

  const changed = probe?.previousFingerprint && probe.previousFingerprint !== probe.fingerprint
  function submit(event: FormEvent) {
    event.preventDefault()
    if (pending) return
    if (step === 'address' && probe) setStep(probe.trusted ? 'auth' : 'trust')
    if (step === 'trust') void trust()
    if (step === 'auth') void authenticate()
  }

  return (
    <dialog
      ref={dialog}
      className="connect-dialog onboarding-dialog"
      aria-labelledby="connect-title"
      onKeyDown={(event) => {
        if (event.key !== 'Tab') return
        const controls = Array.from(event.currentTarget.querySelectorAll<HTMLElement>('*')).filter(
          (element) =>
            element.matches('button, input, summary') &&
            !element.matches(':disabled') &&
            element.getClientRects().length > 0 &&
            (element.tagName === 'SUMMARY' || !element.closest('details:not([open])')),
        )
        if (!controls.length) return
        event.preventDefault()
        const index = controls.indexOf(document.activeElement as HTMLElement)
        const next =
          index < 0
            ? event.shiftKey
              ? controls.length - 1
              : 0
            : (index + (event.shiftKey ? -1 : 1) + controls.length) % controls.length
        controls[next]?.focus()
      }}
      onCancel={(e) => {
        if (pending) e.preventDefault()
        else close(false)
      }}
    >
      <header>
        <div className="dialog-mark">
          <Radio size={18} />
        </div>
        <div>
          <p className="eyebrow">{step === 'address' ? 'Add a server' : probe?.name}</p>
          <h2 id="connect-title" tabIndex={-1}>
            {step === 'address'
              ? 'Where are we meeting?'
              : step === 'trust'
                ? changed
                  ? 'Server certificate changed'
                  : 'Trust this server?'
                : 'Join your community'}
          </h2>
        </div>
        <button
          className="icon-button"
          aria-label="Close"
          disabled={pending}
          onClick={() => close(false)}
        >
          <X size={18} />
        </button>
      </header>
      <form onSubmit={submit}>
        {step === 'address' && (
          <>
            <label>
              Server address
              <input
                required
                disabled={pending}
                placeholder="ss://friends.example:5781"
                value={address}
                onChange={(e) => editAddress(e.target.value)}
              />
            </label>
            <details>
              <summary>Advanced connection options</summary>
              <label>
                HTTPS port
                <input
                  type="number"
                  disabled={pending}
                  min="1"
                  max="65535"
                  placeholder="Same as server port"
                  value={httpsPort}
                  onChange={(e) => editAddress(e.target.value, true)}
                />
              </label>
              <p>
                Domain certificate validation accepts valid renewals automatically. Switching
                replaces an existing fingerprint pin only after both connections validate.
                Disconnect first.
              </p>
              <button
                type="button"
                className="button secondary"
                disabled={pending || checking || !address.trim()}
                onClick={() => void switchDomainTrust()}
              >
                Use domain certificate validation
              </button>
            </details>
            <p role="status">
              {checking
                ? 'Checking server…'
                : probe
                  ? `${probe.name} · ${probe.registrationMode === 'invite' ? 'Invite required' : probe.registrationMode === 'open' ? 'Open registration' : 'Login only'}`
                  : 'Enter the address shared by your server owner.'}
            </p>
          </>
        )}
        {step === 'trust' && probe && (
          <div className={changed ? 'trust-warning' : 'trust-details'}>
            <p>
              {changed
                ? 'The certificate differs from the one you trusted. This could be a server reinstall or someone intercepting your connection. Verify the new fingerprint with the owner before replacing it.'
                : 'Compare this fingerprint with the server owner through a channel you already trust. No login details have been sent.'}
            </p>
            {changed && (
              <>
                <strong>Previously trusted</strong>
                <code>{groupedFingerprint(probe.previousFingerprint!)}</code>
              </>
            )}
            <strong>Server fingerprint (SHA-256)</strong>
            <code>{groupedFingerprint(probe.fingerprint)}</code>
            <small>
              The displayed name is supplied by this server and is not proof of its identity.
            </small>
          </div>
        )}
        {probe?.publicCertificate && (
          <p>Verified domain certificate · Renewals are validated automatically.</p>
        )}
        {step === 'auth' && (
          <>
            <div className="field-row">
              <button
                type="button"
                className="button secondary"
                aria-pressed={mode === 'login'}
                disabled={pending}
                onClick={() => {
                  setMode('login')
                  setError('')
                }}
              >
                Log in
              </button>
              <button
                type="button"
                className="button secondary"
                aria-pressed={mode === 'register'}
                disabled={pending || !['open', 'invite'].includes(probe?.registrationMode ?? '')}
                onClick={() => {
                  setMode('register')
                  setError('')
                }}
              >
                Create account
              </button>
            </div>
            {server && (
              <button
                type="button"
                className="button secondary"
                disabled={pending}
                onClick={() => void authenticate(true)}
              >
                Reconnect with saved session
              </button>
            )}
            <label>
              Username
              <input
                required
                autoComplete="username"
                value={username}
                onChange={(e) => setUsername(e.target.value)}
                disabled={pending}
              />
            </label>
            <label>
              Password
              <input
                required
                type="password"
                autoComplete={mode === 'register' ? 'new-password' : 'current-password'}
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                disabled={pending}
              />
            </label>
            {mode === 'register' && (
              <>
                <small>
                  {password.length >= 16
                    ? 'Good length. Use a unique password for this server.'
                    : 'Use a unique passphrase; 16 or more characters is recommended (8 bytes minimum).'}
                </small>
                <label>
                  Invite code
                  <input
                    required={probe?.registrationMode === 'invite'}
                    autoComplete="off"
                    value={invite}
                    onChange={(e) => setInvite(e.target.value)}
                    disabled={pending}
                  />
                </label>
              </>
            )}
            <small>
              Your session is remembered on this computer. Your password is never saved. Remove the
              server to forget its session.
            </small>
          </>
        )}
        {error && (
          <p className="form-error" role="alert">
            {error}
          </p>
        )}
        {error && step === 'address' && (
          <button
            type="button"
            className="button secondary"
            disabled={checking}
            onClick={() => {
              setError('')
              setRetry((n) => n + 1)
            }}
          >
            Check again
          </button>
        )}
        <div className="dialog-actions">
          <button
            type="button"
            className="button secondary"
            disabled={pending}
            onClick={() => {
              if (step === 'address') close(false)
              else {
                setStep('address')
                setProbe(null)
                setTarget(null)
                setPassword('')
                setError('')
              }
            }}
          >
            {step === 'address' ? 'Cancel' : 'Back'}
          </button>
          <button
            type="submit"
            className={`button primary ${changed && step === 'trust' ? 'danger' : ''}`}
            disabled={pending || (step === 'address' && (!probe || checking))}
          >
            {pending
              ? 'Please wait…'
              : step === 'address'
                ? 'Continue'
                : step === 'trust'
                  ? changed
                    ? 'Replace trusted certificate'
                    : 'Trust this server'
                  : mode === 'register'
                    ? 'Create account and join'
                    : 'Log in and join'}
          </button>
        </div>
      </form>
    </dialog>
  )
}
