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
import { ServerIdentity } from '../../components/ServerIdentity'
import { ProfileSettings } from '../profile/ProfileSettings'
import './onboarding.css'
import { createLocalProfile, getLocalProfile, suggestedUsername } from './localProfile'

type Field = AccountField | 'address' | 'httpsPort' | 'compared' | 'profileName'
type JoinMode = 'profile' | 'login' | 'register'
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
  required = name !== 'httpsPort',
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
  required?: boolean
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
        required={required}
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
  const [step, setStep] = useState<'address' | 'trust' | 'auth' | 'profile'>('address')
  const [chosenMode, setMode] = useState<JoinMode | null>(null)
  const [localProfile, setLocalProfile] = useState<string | null>()
  const [profileName, setProfileName] = useState('')
  const [restoreOpen, setRestoreOpen] = useState(false)
  const [checking, setChecking] = useState(false)
  const [retryDomain, setRetryDomain] = useState(false)
  const [pending, setPending] = useState(false)
  const [error, setError] = useState('')
  const [errors, setErrors] = useState<FieldErrors>({})
  const [advanced, setAdvanced] = useState(false)
  const [compared, setCompared] = useState(false)
  const [enteredUsername, setUsername] = useState<string | null>(null)
  const [password, setPassword] = useState('')
  const [invite, setInvite] = useState('')
  const mounted = useRef(true)
  const revision = useRef(0)
  const canRegister = probe?.registrationMode === 'invite' || probe?.registrationMode === 'open'
  const changedPin = !!probe?.previousFingerprint && probe.previousFingerprint !== probe.fingerprint
  const savedServer = target && servers.find((s) => matchesServer(s, target))
  const knownUsername = savedServer?.profileUsername
  const profileSupported =
    localProfile !== undefined && probe?.capabilities?.includes('profileSignIn') === true
  const legacyMode = savedServer || !canRegister ? 'login' : 'register'
  const mode =
    chosenMode ??
    (profileSupported && (knownUsername || (!savedServer && (canRegister || localProfile)))
      ? 'profile'
      : legacyMode)
  const wantsProfile = mode === 'profile'
  const needsInvite =
    mode !== 'login' && !(wantsProfile && knownUsername) && probe?.registrationMode === 'invite'
  const username =
    wantsProfile && knownUsername
      ? knownUsername
      : (enteredUsername ?? (wantsProfile ? suggestedUsername(localProfile) : ''))
  const invalidFocus = useRef<Field | undefined>(undefined)
  const form = useRef<HTMLFormElement>(null)

  useEffect(() => {
    mounted.current = true
    let active = true
    void getLocalProfile()
      .then((name) => {
        if (active) setLocalProfile(name)
      })
      .catch(() => {
        // Platforms without a profile store still have server-account sign-in.
      })
    const previous = document.activeElement as HTMLElement | null
    const modal = dialog.current
    modal?.showModal()
    return () => {
      active = false
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
    else if (step === 'auth' && wantsProfile && (!localProfile || knownUsername))
      dialog.current?.querySelector<HTMLElement>('h2')?.focus()
    else
      focusField(step === 'address' ? 'address' : step === 'profile' ? 'profileName' : 'username')
  }, [step, mode, wantsProfile, localProfile, knownUsername])
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
      setMode(null)
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
  function openProfileSetup() {
    setError('')
    setErrors({})
    setStep('profile')
  }
  async function saveProfile() {
    const name = profileName.trim()
    if (!name || [...name].length > 64) {
      reportErrors({ profileName: 'Choose a profile name between 1 and 64 characters.' })
      return
    }
    setPending(true)
    setError('')
    try {
      const created = await createLocalProfile(name)
      if (mounted.current) {
        setLocalProfile(created)
        setStep(target ? 'auth' : 'address')
      }
    } catch (e) {
      if (mounted.current) setError(friendlyError(e))
    } finally {
      if (mounted.current) setPending(false)
    }
  }
  function switchMode(next: JoinMode) {
    setMode(next)
    setPassword('')
    setErrors({})
    setError('')
  }
  async function authenticate(saved = false) {
    if (!target || !probe || pending) return
    if (!saved && wantsProfile && !localProfile) {
      openProfileSetup()
      return
    }
    // A returning member can prove ownership without another invitation.
    const invalid = saved
      ? {}
      : accountErrors(
          username,
          wantsProfile ? null : password,
          mode !== 'login',
          needsInvite && !wantsProfile,
          invite,
        )
    reportErrors(invalid)
    if (Object.keys(invalid).length) return
    setPending(true)
    setError('')
    try {
      const result = await connect({
        ...target,
        username: saved ? '' : username.trim(),
        password: saved || wantsProfile ? '' : password,
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
        // Rediscover the replacement certificate's name, logo and capabilities.
        changeServer()
        await findServer()
      } else if (
        !saved &&
        needsInvite &&
        failure.code === 'server_4' &&
        /invite/i.test(failure.message ?? '')
      ) {
        reportErrors({ invite: 'This invite was rejected. Ask your host for a new code.' })
      } else if (!saved && mode !== 'login' && failure.code === 'server_6') {
        reportErrors({ username: friendlyError(e) })
      } else if (!saved && wantsProfile && failure.code === 'server_1') {
        setError(
          'This server did not accept your profile. If you have a server account, use its login instead.',
        )
      } else if (!saved && failure.code === 'server_1') {
        reportErrors({ password: friendlyError(e) })
      } else setError(friendlyError(e))
    } finally {
      if (mounted.current) setPending(false)
    }
  }
  function changeServer() {
    setStep('address')
    setMode(null)
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
    if (step === 'profile') void saveProfile()
    else if (step === 'address') void findServer(retryDomain)
    else if (step === 'trust') void trust()
    else void authenticate()
  }

  const { title, description, action } = {
    profile: {
      title: 'Choose your profile name',
      description:
        'Sideband keeps your profile on this computer. Use it to join servers that support profile sign-in.',
      action: 'Create profile',
    },
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
    auth: wantsProfile
      ? {
          title: localProfile
            ? `Join ${probe?.name} with your profile`
            : 'Join with a local profile',
          description: knownUsername
            ? `Sign in again as ${knownUsername}.`
            : localProfile
              ? `Use ${localProfile} to sign in. Your membership belongs to this server.`
              : 'Choose a name once and use your profile to sign in to compatible servers.',
          action: knownUsername
            ? 'Sign in with profile'
            : localProfile
              ? 'Join with profile'
              : 'Create a local profile',
        }
      : {
          title: mode === 'register' ? 'Create your account' : `Log in to ${probe?.name}`,
          description: canRegister
            ? 'Your account belongs to this server.'
            : 'Registration is closed. Use your existing account on this server.',
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
        if (e.key !== 'Tab' || (e.target as HTMLElement).closest('dialog') !== e.currentTarget)
          return
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
      {restoreOpen && (
        <ProfileSettings
          initialSection="restore"
          close={() => setRestoreOpen(false)}
          onRestored={(name) => {
            setLocalProfile(name)
            setRestoreOpen(false)
            if (useAppStore.getState().servers.length > 0) close(false)
          }}
        />
      )}
      <header className="join-header">
        <span className="join-brand">
          <img src="/brand/sideband-mark.svg" alt="" width="28" height="28" />
          Sideband
        </span>
        <ol aria-label="Joining a server">
          {(step === 'profile'
            ? ['Profile', 'Server', 'Chat']
            : ['Server', wantsProfile ? 'Profile' : 'Account', 'Chat']
          ).map((label, i) => (
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
            {step === 'auth' && target && probe && (
              <ServerIdentity
                name={probe.name}
                target={target}
                hasPublicIcon={probe.hasPublicIcon}
              />
            )}
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
            {step === 'profile' && (
              <JoinField
                name="profileName"
                label="Profile name"
                value={profileName}
                onChange={setProfileName}
                autoComplete="nickname"
                issue={errors.profileName}
                disabled={pending}
              />
            )}
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
                {savedServer && !wantsProfile && (
                  <button
                    type="button"
                    className="join-link"
                    disabled={pending}
                    onClick={() => void authenticate(true)}
                  >
                    Reconnect with saved session
                  </button>
                )}
                {(!wantsProfile || (localProfile && !knownUsername)) && (
                  <JoinField
                    name="username"
                    label="Username"
                    autoComplete="username"
                    value={username}
                    onChange={setUsername}
                    disabled={pending}
                    issue={errors.username}
                    hint={
                      mode !== 'login'
                        ? '3–32 lowercase letters, digits, underscores or dots.'
                        : undefined
                    }
                  />
                )}
                {!wantsProfile && (
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
                )}
                {needsInvite && (!wantsProfile || localProfile) && (
                  <JoinField
                    name="invite"
                    label="Invite code"
                    required={!wantsProfile}
                    hint={
                      wantsProfile
                        ? 'Use the code from your host when joining for the first time.'
                        : undefined
                    }
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
              {step === 'address' && localProfile === null && (
                <button
                  type="button"
                  className="join-link"
                  disabled={pending || checking}
                  onClick={openProfileSetup}
                >
                  Create a local profile
                </button>
              )}
              {step === 'address' && localProfile === null && (
                <button
                  type="button"
                  className="join-link"
                  disabled={pending || checking}
                  onClick={() => setRestoreOpen(true)}
                >
                  Restore from backup
                </button>
              )}
              {step === 'auth' && profileSupported && (canRegister || localProfile) && (
                <button
                  type="button"
                  className="join-link"
                  disabled={pending}
                  onClick={() => switchMode(wantsProfile ? 'login' : 'profile')}
                >
                  {wantsProfile ? 'Use a server account' : 'Use a local profile'}
                </button>
              )}
              {step === 'auth' && !wantsProfile && canRegister && (
                <button
                  type="button"
                  className="join-link"
                  disabled={pending}
                  onClick={() => switchMode(mode === 'login' ? 'register' : 'login')}
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
                onClick={() => {
                  if (step === 'profile' && target) {
                    setStep('auth')
                    setError('')
                    setErrors({})
                  } else changeServer()
                }}
              >
                {step === 'trust' || step === 'profile' ? 'Go back' : 'Change server'}
              </button>
            )}
          </div>
        </div>
      </form>
    </dialog>
  )
}
