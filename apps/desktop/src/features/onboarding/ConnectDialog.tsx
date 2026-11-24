import { useEffect, useRef, useState, type FormEvent } from 'react'
import { ChevronRight, X } from 'lucide-react'
import {
  approveFingerprint,
  connect,
  getSnapshot,
  probeServer,
  enablePublicCertificate,
} from '../../lib/api'
import type { ApiError, ProbeInput, ServerProbe, ServerSummary } from '../../lib/bindings'
import { useAppStore } from '../../store/app-store'
import {
  accountErrors,
  friendlyError,
  groupedFingerprint,
  InvalidHttpsPort,
  parseAddress,
  type AccountField,
} from './validation'
import './onboarding.css'

type Field = AccountField | 'address' | 'httpsPort' | 'compared'
type FieldErrors = Partial<Record<Field, string>>

function matchesServer(server: ServerSummary, input: ProbeInput) {
  return server.host.toLowerCase() === input.host && server.port === input.port
}

function JoinField({
  name,
  label,
  value,
  onChange,
  hint,
  placeholder,
  issue,
  disabled,
  autoComplete = 'off',
}: {
  name: Exclude<Field, 'compared'>
  label: string
  value: string
  onChange: (value: string) => void
  hint?: string
  placeholder?: string
  issue?: string
  disabled: boolean
  autoComplete?: string
}) {
  return (
    <div className="join-field">
      <label htmlFor={`join-${name}`}>{label}</label>
      <input
        id={`join-${name}`}
        name={name}
        type={name === 'password' ? 'password' : 'text'}
        value={value}
        disabled={disabled}
        required={name !== 'httpsPort'}
        inputMode={name === 'httpsPort' ? 'numeric' : undefined}
        spellCheck={false}
        autoCapitalize="none"
        autoComplete={autoComplete}
        aria-invalid={!!issue}
        aria-describedby={issue || hint ? `join-${name}-hint` : undefined}
        placeholder={placeholder}
        onChange={(e) => onChange(e.target.value)}
      />
      {(issue || hint) && (
        <p
          id={`join-${name}-hint`}
          className={issue ? 'join-error' : undefined}
          role={issue ? 'alert' : undefined}
        >
          {issue || hint}
        </p>
      )}
    </div>
  )
}

export function ConnectDialog() {
  const open = useAppStore((s) => s.connectOpen)
  const server = useAppStore((s) => s.connectServer)
  return open ? <Onboarding server={server} /> : null
}

function Onboarding({ server }: { server: ServerSummary | null }) {
  const close = useAppStore((s) => s.setConnectOpen)
  const servers = useAppStore((s) => s.servers)
  const firstServer = servers.length === 0
  const add = useAppStore((s) => s.addConnection)
  const dialog = useRef<HTMLDialogElement>(null)
  const [address, setAddress] = useState(
    server ? `${server.host.includes(':') ? `[${server.host}]` : server.host}:${server.port}` : '',
  )
  const [httpsPort, setHttpsPort] = useState(
    server && server.httpsPort !== server.port ? String(server.httpsPort) : '',
  )
  const [target, setTarget] = useState<ProbeInput | null>(null)
  const [probe, setProbe] = useState<ServerProbe | null>(null)
  const [step, setStep] = useState<'address' | 'trust' | 'auth'>('address')
  const [mode, setMode] = useState<'login' | 'register'>('register')
  const [checking, setChecking] = useState(false)
  const [retryDomain, setRetryDomain] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const [errors, setErrors] = useState<FieldErrors>({})
  const [advanced, setAdvanced] = useState(false)
  const [compared, setCompared] = useState(false)
  const [username, setUsername] = useState('')
  const [password, setPassword] = useState('')
  const [invite, setInvite] = useState('')
  const mounted = useRef(true)
  const revision = useRef(0)
  const canRegister = probe?.registrationMode === 'invite' || probe?.registrationMode === 'open'
  const needsInvite = mode === 'register' && probe?.registrationMode === 'invite'
  const changedPin = !!probe?.previousFingerprint && probe.previousFingerprint !== probe.fingerprint
  const savedServer = target && servers.some((s) => matchesServer(s, target))
  const invalidFocus = useRef<Field | undefined>(undefined)
  const form = useRef<HTMLFormElement>(null)

  useEffect(() => {
    mounted.current = true
    const previous = document.activeElement as HTMLElement | null
    const modal = dialog.current
    modal?.showModal()
    return () => {
      mounted.current = false
      modal?.close()
      if (previous?.isConnected && previous !== document.body) previous.focus()
      else if (!useAppStore.getState().servers.length)
        document.querySelector<HTMLButtonElement>('.add-server')?.focus()
    }
  }, [])

  function focusField(name: Field) {
    const input = form.current?.elements.namedItem(name)
    if (input instanceof HTMLInputElement) input.focus()
  }
  useEffect(() => {
    if (step === 'trust') dialog.current?.querySelector<HTMLElement>('h2')?.focus()
    else focusField(step === 'address' ? 'address' : 'username')
  }, [step, mode])
  // After a native request, wait until the inputs are enabled again before focusing the error.
  useEffect(() => {
    if (invalidFocus.current && !pending) {
      focusField(invalidFocus.current)
      invalidFocus.current = undefined
    }
  }, [errors, pending])

  function reportErrors(issues: FieldErrors) {
    invalidFocus.current = Object.keys(issues)[0] as Field | undefined
    setErrors(issues)
  }
  function editAddress(value: string, https = false) {
    revision.current++
    setChecking(false)
    setRetryDomain(false)
    setError('')
    setErrors({})
    if (https) setHttpsPort(value)
    else setAddress(value)
  }
  async function findServer(domain = false) {
    if (checking || pending) return
    setError('')
    setErrors({})
    let input: ProbeInput
    try {
      input = parseAddress(address, httpsPort)
    } catch (e) {
      const field = e instanceof InvalidHttpsPort ? 'httpsPort' : 'address'
      if (field === 'httpsPort') setAdvanced(true)
      reportErrors({
        [field]: address.trim() ? friendlyError(e) : 'Enter the server address from your host.',
      })
      return
    }
    const request = ++revision.current
    setChecking(true)
    // Switching from a saved pin changes trust on disk. Keep its target fixed until it completes.
    if (domain) setPending(true)
    try {
      const found = await (domain ? enablePublicCertificate(input) : probeServer(input))
      if (!mounted.current || request !== revision.current) return
      setRetryDomain(false)
      setProbe(found)
      setTarget(input)
      setCompared(false)
      const returning = servers.some((s) => matchesServer(s, input))
      setMode(
        returning || !['open', 'invite'].includes(found.registrationMode) ? 'login' : 'register',
      )
      setStep(found.trusted ? 'auth' : 'trust')
    } catch (e) {
      if (mounted.current && request === revision.current) {
        setRetryDomain(domain)
        setError(friendlyError(e))
      }
    } finally {
      if (mounted.current && request === revision.current) {
        setChecking(false)
        setPending(false)
      }
    }
  }
  async function trust() {
    if (!probe || !target) return
    if (!compared) {
      reportErrors({ compared: 'Compare the fingerprint with your host before continuing.' })
      return
    }
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
  async function authenticate(saved = false) {
    if (!target || !probe || pending) return
    const invalid = saved
      ? {}
      : accountErrors(username, password, mode === 'register', needsInvite, invite)
    reportErrors(invalid)
    if (Object.keys(invalid).length) return
    setPending(true)
    setError('')
    try {
      const result = await connect({
        ...target,
        username: saved ? '' : username.trim(),
        password: saved ? '' : password,
        mode: saved ? 'saved' : mode,
        inviteCode: !saved && needsInvite ? invite.trim() : '',
      })
      if (mounted.current) {
        setPassword('')
        add(result, target)
        void getSnapshot()
          .then((s) => useAppStore.getState().hydrate(s.servers))
          .catch(() => undefined)
      }
    } catch (e) {
      if (!mounted.current) return
      const failure = e as Partial<ApiError>
      setPassword('')
      if (failure.fingerprint) {
        setProbe({
          ...probe,
          trusted: false,
          publicCertificate: false,
          previousFingerprint: probe.fingerprint,
          fingerprint: failure.fingerprint,
        })
        setCompared(false)
        setStep('trust')
      } else if (
        !saved &&
        needsInvite &&
        failure.code === 'server_4' &&
        /invite/i.test(failure.message ?? '')
      ) {
        reportErrors({ invite: 'This invite was rejected. Ask your host for a new code.' })
      } else if (!saved && mode === 'register' && failure.code === 'server_6') {
        reportErrors({ username: friendlyError(e) })
      } else if (!saved && failure.code === 'server_1') {
        reportErrors({ password: friendlyError(e) })
      } else setError(friendlyError(e))
    } finally {
      if (mounted.current) setPending(false)
    }
  }
  function changeServer() {
    setStep('address')
    setProbe(null)
    setTarget(null)
    setCompared(false)
    setPassword('')
    setInvite('')
    setError('')
    setErrors({})
  }
  function submit(event: FormEvent) {
    event.preventDefault()
    if (pending || checking) return
    if (step === 'address') void findServer(retryDomain)
    else if (step === 'trust') void trust()
    else void authenticate()
  }

  const { title, description, action } = {
    address: {
      title: server
        ? 'Reconnect to your server'
        : firstServer
          ? 'Join your first server'
          : 'Add a server',
      description: 'Enter the server address your host shared with you.',
      action: retryDomain ? 'Retry domain validation' : error ? 'Try again' : 'Find server',
    },
    trust: {
      title: changedPin ? 'Server certificate changed' : 'Check the server fingerprint',
      description: changedPin
        ? 'The certificate differs from the one you trusted. This could be a server reinstall or someone intercepting your connection. Verify the new fingerprint with the owner before replacing it.'
        : `Ask the host of ${address.trim()} for this fingerprint through a conversation you already trust. No login details have been sent.`,
      action: changedPin ? 'Replace trusted certificate' : 'Trust this server',
    },
    auth: {
      title: mode === 'register' ? 'Create your account' : `Log in to ${probe?.name}`,
      description: canRegister
        ? `Your account belongs to ${probe?.name} · ${address.trim()}`
        : `Registration is closed. Use your existing account on ${probe?.name} (${address.trim()}).`,
      action: mode === 'register' ? 'Create account and join' : 'Log in and join',
    },
  }[step]
  return (
    <dialog
      ref={dialog}
      className="join-server"
      data-first={firstServer || undefined}
      aria-labelledby="connect-title"
      onCancel={(e) => {
        e.preventDefault()
        if (!pending) close(false)
      }}
      onKeyDown={(e) => {
        if (e.key !== 'Tab') return
        const controls = Array.from(
          e.currentTarget.querySelectorAll<HTMLElement>('button,input,summary'),
        ).filter(
          (el) =>
            !el.matches(':disabled') &&
            (el.tagName === 'SUMMARY' || !el.closest('details:not([open])')) &&
            el.getClientRects().length,
        )
        if (!controls.length) return
        const index = controls.indexOf(document.activeElement as HTMLElement)
        e.preventDefault()
        const next =
          index < 0
            ? e.shiftKey
              ? controls.length - 1
              : 0
            : (index + controls.length + (e.shiftKey ? -1 : 1)) % controls.length
        controls[next]?.focus()
      }}
    >
      <header className="join-header">
        <span className="join-brand">
          <img src="/brand/sideband-mark.svg" alt="" width="28" height="28" />
          Sideband
        </span>
        <ol aria-label="Joining a server">
          {['Server', 'Account', 'Chat'].map((label, i) => (
            <li key={label} aria-current={(step === 'auth' ? 1 : 0) === i ? 'step' : undefined}>
              {label}
            </li>
          ))}
        </ol>
        <button
          type="button"
          className="join-close"
          aria-label="Close"
          disabled={pending}
          onClick={() => close(false)}
        >
          <X size={18} />
        </button>
      </header>
      <form
        ref={form}
        onSubmit={submit}
        noValidate
        onChange={(e) => {
          const input = e.target
          if (!(input instanceof HTMLInputElement)) return
          const field = input.name as Field
          if (errors[field]) setErrors((current) => ({ ...current, [field]: undefined }))
        }}
      >
        <div className="join-scroll" key={step}>
          <div className="join-content">
            {step === 'auth' && (
              <p className="join-certificate">
                {probe?.publicCertificate
                  ? 'Verified domain certificate'
                  : 'Trusted server fingerprint'}
              </p>
            )}
            <h2 tabIndex={-1} id="connect-title">
              {title}
            </h2>
            <p className="join-intro">{description}</p>
            {step === 'address' && (
              <>
                <JoinField
                  name="address"
                  label="Server address"
                  value={address}
                  onChange={editAddress}
                  placeholder="voice.example.com"
                  issue={errors.address}
                  disabled={pending}
                />
                <details open={advanced} onToggle={(e) => setAdvanced(e.currentTarget.open)}>
                  <summary>
                    <ChevronRight size={16} />
                    Advanced connection options
                  </summary>
                  <JoinField
                    name="httpsPort"
                    label="HTTPS port"
                    value={httpsPort}
                    onChange={(v) => editAddress(v, true)}
                    placeholder="Same as server port"
                    hint="Change this only if your host gave you a separate HTTPS port."
                    issue={errors.httpsPort}
                    disabled={pending}
                  />
                  <p>
                    Domain validation accepts valid certificate renewals. Disconnect before
                    switching an existing fingerprint pin.
                  </p>
                  <button
                    type="button"
                    className="join-link"
                    disabled={pending || checking || !address.trim()}
                    onClick={() => void findServer(true)}
                  >
                    Use domain certificate validation
                  </button>
                </details>
              </>
            )}
            {step === 'trust' && probe && (
              <>
                {changedPin && (
                  <div className="join-fingerprint">
                    <strong>Previously trusted</strong>
                    <code>{groupedFingerprint(probe.previousFingerprint!)}</code>
                  </div>
                )}
                <div className="join-fingerprint">
                  <strong>Server fingerprint (SHA-256)</strong>
                  <code>{groupedFingerprint(probe.fingerprint)}</code>
                </div>
                <label className="join-compare">
                  <input
                    type="checkbox"
                    name="compared"
                    checked={compared}
                    disabled={pending}
                    aria-invalid={!!errors.compared}
                    aria-describedby={errors.compared ? 'compare-error' : undefined}
                    onChange={(e) => {
                      setCompared(e.target.checked)
                      setErrors({})
                    }}
                  />
                  I compared the fingerprint and it matches.
                </label>
                {errors.compared && (
                  <p id="compare-error" role="alert" className="join-error">
                    {errors.compared}
                  </p>
                )}
              </>
            )}
            {step === 'auth' && (
              <>
                {savedServer && (
                  <button
                    type="button"
                    className="join-link"
                    disabled={pending}
                    onClick={() => void authenticate(true)}
                  >
                    Reconnect with saved session
                  </button>
                )}
                <JoinField
                  name="username"
                  label="Username"
                  autoComplete="username"
                  value={username}
                  onChange={setUsername}
                  disabled={pending}
                  issue={errors.username}
                  hint={
                    mode === 'register'
                      ? '3–32 lowercase letters, digits, underscores or dots.'
                      : undefined
                  }
                />
                <JoinField
                  name="password"
                  label="Password"
                  autoComplete={mode === 'register' ? 'new-password' : 'current-password'}
                  value={password}
                  onChange={setPassword}
                  disabled={pending}
                  issue={errors.password}
                  hint={
                    mode === 'register'
                      ? 'Use a unique password. Aim for 16 or more characters.'
                      : undefined
                  }
                />
                {needsInvite && (
                  <JoinField
                    name="invite"
                    label="Invite code"
                    value={invite}
                    onChange={setInvite}
                    disabled={pending}
                    issue={errors.invite}
                  />
                )}
              </>
            )}
            {error && (
              <p role="alert" className="join-error">
                {error}
              </p>
            )}
            <div className="join-actions">
              <button
                className={'join-primary' + (step === 'trust' && changedPin ? ' join-replace' : '')}
                type="submit"
                disabled={checking || pending}
              >
                {checking ? 'Checking server…' : pending ? 'Please wait…' : action}
              </button>
              {step === 'auth' && canRegister && (
                <button
                  type="button"
                  className="join-link"
                  disabled={pending}
                  onClick={() => {
                    setMode(mode === 'login' ? 'register' : 'login')
                    setPassword('')
                    setErrors({})
                    setError('')
                  }}
                >
                  {mode === 'register' ? 'Log in instead' : 'Create an account'}
                </button>
              )}
            </div>
            {checking && (
              <span className="sr-only" role="status">
                Checking server…
              </span>
            )}
            {step === 'address' ? (
              <details className="join-help">
                <summary>Where do I get an address?</summary>
                <p>
                  Ask the person who invited you for the server address. If it includes a port,
                  paste the whole address. Your host can also give you an invite code if the server
                  requires one.
                </p>
              </details>
            ) : (
              <button
                className="join-link join-back"
                type="button"
                disabled={pending}
                onClick={changeServer}
              >
                {step === 'trust' ? 'Go back' : 'Change server'}
              </button>
            )}
          </div>
        </div>
      </form>
    </dialog>
  )
}
