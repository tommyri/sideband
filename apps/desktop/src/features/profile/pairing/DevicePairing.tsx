import { useEffect, useRef, useState } from 'react'
import { ShieldCheck } from 'lucide-react'
import type { PairView } from '../../../lib/generated/PairView'
import { friendlyError } from '../../onboarding/validation'
import {
  startPairing,
  joinPairing,
  updatePairing,
  pairingLink,
  readInvitation,
  relayAddress,
} from './api'
import './pairing.css'

type Props = {
  source: boolean
  invitation?: string
  onInstalling?: (busy: boolean) => void
  onLinked: (name: string) => void
  onChooseRelay: () => void
  onClose: () => void
}
function invitationAddress(input: string) {
  try {
    return relayAddress(readInvitation(input, '').target)
  } catch {
    return ''
  }
}
const defaultDevice = () =>
  /Windows/i.test(navigator.userAgent)
    ? 'Windows PC'
    : /Mac/i.test(navigator.userAgent)
      ? 'Mac'
      : 'Desktop computer'

export function DevicePairing({
  source,
  invitation = '',
  onInstalling,
  onLinked,
  onChooseRelay,
  onClose,
}: Props) {
  const [view, setView] = useState<PairView | null>(null)
  const [input, setInput] = useState(invitation),
    [code, setCode] = useState(''),
    [name, setName] = useState(defaultDevice)
  const [pending, setPending] = useState(false),
    [error, setError] = useState(''),
    [matched, setMatched] = useState(false),
    [copied, setCopied] = useState(false)
  const session = useRef(''),
    mounted = useRef(true)
  const kind = view?.kind
  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
      const id = session.current
      session.current = ''
      if (id) void updatePairing(id, 'cancel').catch(() => {})
    }
  }, [])
  function fail(e: unknown) {
    setError(friendlyError(e))
    const code = (e as { code?: string })?.code
    if (['pair_invalid', 'pair_ended', 'pair_changed'].includes(code ?? '')) {
      const id = session.current
      session.current = ''
      if (id) void updatePairing(id, 'cancel').catch(() => {})
      setView(null)
      setMatched(false)
    }
  }
  useEffect(() => {
    if (!['invite', 'compare', 'review'].includes(kind ?? '') || pending) return
    const id = session.current
    let active = true
    let timeout: ReturnType<typeof setTimeout>
    async function poll() {
      try {
        const result = await updatePairing(id, 'check')
        if (!active || session.current !== id) return
        setError('')
        if (result.kind !== 'waiting') setView(result)
      } catch (e) {
        if (!active || session.current !== id) return
        fail(e)
      }
      if (active && session.current === id) timeout = setTimeout(() => void poll(), 1500)
    }
    timeout = setTimeout(() => void poll(), 800)
    return () => {
      active = false
      clearTimeout(timeout)
    }
  }, [kind, pending])

  async function begin() {
    const id = crypto.randomUUID()
    session.current = id
    setPending(true)
    setError('')
    setMatched(false)
    setCopied(false)
    try {
      let result: PairView
      if (source) {
        result = await startPairing(id)
      } else {
        const details = readInvitation(input, code)
        result = await joinPairing(id, details.target, details.code, name.trim())
      }
      if (!mounted.current || session.current !== id) {
        void updatePairing(id, 'cancel').catch(() => {})
        return
      }
      setView(result)
    } catch (e) {
      if (mounted.current && session.current === id) {
        session.current = ''
        setView(null)
        setError(friendlyError(e))
      }
    } finally {
      if (mounted.current && (!session.current || session.current === id)) setPending(false)
    }
  }
  async function act(action: 'install' | 'approve' | 'cancel') {
    const id = session.current
    if (!id) return
    if (action === 'install') onInstalling?.(true)
    setPending(true)
    setError('')
    try {
      const next = await updatePairing(id, action)
      if (!mounted.current || session.current !== id) return
      session.current = ''
      setView(next)
    } catch (e) {
      if (mounted.current) fail(e)
    } finally {
      if (mounted.current) {
        setPending(false)
        if (action === 'install') onInstalling?.(false)
      }
    }
  }
  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text)
      setCopied(true)
    } catch {
      setError('Select the link and copy it using your keyboard.')
    }
  }
  const link = view?.kind === 'invite' ? pairingLink(view.target, view.code) : ''
  const hasLink = input.trim().toLowerCase().startsWith('sideband:')
  const destination = hasLink ? invitationAddress(input) : ''
  return (
    <section className="device-pairing">
      {!view && (
        <>
          <h3>{source ? 'Link a device' : 'Link this computer'}</h3>
          {source ? (
            <>
              <p>
                Create a code on your chosen sync server. Keep this window open while you link the
                other computer.
              </p>
              <div className="profile-actions">
                <button className="button primary" disabled={pending} onClick={() => void begin()}>
                  {pending ? 'Creating code…' : 'Create one-time code'}
                </button>
                <button className="button secondary" disabled={pending} onClick={onChooseRelay}>
                  Choose sync server
                </button>
              </div>
            </>
          ) : (
            <form
              onSubmit={(e) => {
                e.preventDefault()
                void begin()
              }}
            >
              <p>On your trusted computer, open Profile and devices and choose Link a device.</p>
              <label>
                Pairing link or sync server address
                <input
                  value={input}
                  onChange={(e) => setInput(e.target.value)}
                  autoComplete="off"
                  spellCheck={false}
                  required
                />
              </label>
              {destination && (
                <p>
                  Sync server <strong>{destination}</strong>
                </p>
              )}
              {!hasLink && (
                <label>
                  One-time code
                  <input
                    value={code}
                    onChange={(e) => setCode(e.target.value)}
                    autoComplete="off"
                    spellCheck={false}
                    required
                  />
                </label>
              )}
              <label>
                Name of this computer
                <input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  maxLength={64}
                  required
                />
              </label>
              <button
                className="button primary"
                disabled={pending || !name.trim() || !input.trim() || (!hasLink && !code.trim())}
              >
                {pending ? 'Requesting access…' : 'Request access'}
              </button>
            </form>
          )}
        </>
      )}
      {view?.kind === 'invite' && (
        <>
          <h3>Open this link on your other computer</h3>
          <p>This invitation lasts five minutes. You’ll review the request here.</p>
          <label>
            Pairing link
            <input readOnly value={link} onFocus={(e) => e.currentTarget.select()} />
          </label>
          <button className="button secondary" onClick={() => void copy(link)}>
            {copied ? 'Link copied' : 'Copy pairing link'}
          </button>
          <p>
            Or choose <strong>Link this computer</strong> on the other device and enter both
            details.
          </p>
          <label>
            Sync server address
            <input readOnly value={relayAddress(view.target)} />
          </label>
          <label>
            One-time code
            <input readOnly value={view.code} />
          </label>
          <div className="profile-actions">
            <button className="button secondary" disabled={pending} onClick={() => void begin()}>
              Create new code
            </button>
            <button
              className="button secondary"
              disabled={pending}
              onClick={() => void act('cancel')}
            >
              Cancel linking
            </button>
          </div>
        </>
      )}
      {(view?.kind === 'review' || view?.kind === 'compare') && (
        <>
          <h3>{source ? 'Allow ' + view.device + '?' : 'Check the phrase'}</h3>
          <p>
            {view.kind === 'review'
              ? view.device +
                ' will gain access to your ' +
                view.memberships +
                ' server membership' +
                (view.memberships === 1 ? '' : 's') +
                '.'
              : 'You’re linking ' +
                view.device +
                ' to the profile and its server memberships.'}{' '}
            Compare these words on both computers.
          </p>
          <div className="pairing-phrase">
            <ShieldCheck size={18} aria-hidden="true" />
            <span>
              Matching phrase<strong>{view.phrase}</strong>
            </span>
          </div>
          {source ? (
            <>
              <label className="pairing-match">
                <input
                  type="checkbox"
                  checked={matched}
                  onChange={(e) => setMatched(e.target.checked)}
                />
                The words match on both computers
              </label>
              <div className="profile-actions">
                <button
                  className="button primary"
                  disabled={!matched || pending}
                  onClick={() => void act('approve')}
                >
                  Allow {view.device}
                </button>
                <button
                  className="button secondary"
                  disabled={pending}
                  onClick={() => void act('cancel')}
                >
                  Deny request
                </button>
              </div>
            </>
          ) : (
            <>
              <p role="status">
                Waiting for approval on your trusted computer. Cancel if the words differ.
              </p>
              <button
                className="button secondary"
                disabled={pending}
                onClick={() => void act('cancel')}
              >
                Cancel linking
              </button>
            </>
          )}
        </>
      )}
      {kind === 'ready' && (
        <>
          <h3>Your trusted computer approved</h3>
          <p>
            Finish linking to use your profile on this computer. Your microphone, push-to-talk and
            notification settings stay local.
          </p>
          <div className="profile-actions">
            <button
              className="button primary"
              disabled={pending}
              onClick={() => void act('install')}
            >
              {pending ? 'Linking…' : 'Finish linking'}
            </button>
            <button
              className="button secondary"
              disabled={pending}
              onClick={() => void act('cancel')}
            >
              Cancel linking
            </button>
          </div>
        </>
      )}
      {kind === 'allowed' && (
        <>
          <h3>Access approved</h3>
          <p>Choose Finish linking on the other computer before the invitation expires.</p>
          <button className="button primary" onClick={onClose}>
            Done
          </button>
        </>
      )}
      {view?.kind === 'linked' && (
        <>
          <h3>This computer is linked</h3>
          <p>Open a server and sign in with your profile to reconnect as {view.name}.</p>
          <button className="button primary" onClick={() => onLinked(view.name)}>
            Continue
          </button>
        </>
      )}
      {kind === 'closed' && (
        <>
          <h3>Linking canceled</h3>
          <p>This invitation has ended.</p>
          <button
            className="button secondary"
            onClick={() => {
              setView(null)
              setMatched(false)
            }}
          >
            Start again
          </button>
        </>
      )}
      {error && <p role="alert">{error}</p>}
    </section>
  )
}
