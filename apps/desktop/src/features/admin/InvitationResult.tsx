import { useEffect, useRef, useState } from 'react'
import type { ServerSummary } from '../../lib/generated/ServerSummary'
import type { CreatedInvite } from '../../lib/generated/CreatedInvite'
import { DEFAULT_PORT, parseAddress } from '../onboarding/validation'

function addressText(host: string, port: number) {
  return (host.includes(':') ? `[${host}]` : host) + (port === DEFAULT_PORT ? '' : `:${port}`)
}

function localAddress(host: string) {
  const address = host.toLowerCase().replace(/\.$/, '')
  return (
    address === 'localhost' ||
    address.endsWith('.localhost') ||
    /^127\./.test(address) ||
    ['0.0.0.0', '::', '::1'].includes(address)
  )
}

export function InvitationResult({
  server,
  invite,
  done,
}: {
  server: ServerSummary
  invite: CreatedInvite
  done: () => void
}) {
  const [address, setAddress] = useState(
    localAddress(server.host) ? '' : addressText(server.host, server.port),
  )
  const [httpsPort, setHttpsPort] = useState(
    server.httpsPort === server.port ? '' : String(server.httpsPort),
  )
  const [editing, setEditing] = useState(localAddress(server.host))
  const [status, setStatus] = useState<'ready' | 'copied' | 'manual'>('ready')
  const manual = useRef<HTMLTextAreaElement>(null)
  const heading = useRef<HTMLHeadingElement>(null)
  useEffect(() => {
    heading.current?.focus()
  }, [])
  useEffect(() => {
    if (status === 'manual') {
      manual.current?.focus()
      manual.current?.select()
    }
  }, [status])
  let target
  let problem = ''
  try {
    target = parseAddress(address, httpsPort, server.port)
    if (localAddress(target.host)) problem = 'Enter the server address your guest can reach.'
  } catch (error) {
    problem = error instanceof Error ? error.message : 'Check the server address.'
  }
  const fields = [
    [
      'Server',
      target && !problem ? addressText(target.host, target.port) : 'Enter a shared address',
    ],
  ]
  if (target && target.httpsPort !== target.port)
    fields.push(['Advanced HTTPS port', String(target.httpsPort)])
  fields.push(['Invite code', invite.code])
  if (invite.expiresAt != null)
    fields.push([
      'Expires',
      new Date(invite.expiresAt).toLocaleString(undefined, {
        dateStyle: 'medium',
        timeStyle: 'long',
      }),
    ])
  const title = `Join ${server.label} on Sideband`
  const instruction = 'Open Sideband, add this server and create an account with the code below.'
  const invitation = [
    title,
    instruction,
    '',
    ...fields.map(([label, value]) => `${label}: ${value}`),
  ].join('\n')
  async function copy() {
    try {
      await navigator.clipboard.writeText(invitation)
      setStatus('copied')
    } catch {
      setStatus('manual')
      manual.current?.select()
    }
  }
  return (
    <section className="invitation-result" aria-labelledby="invitation-heading">
      <h2 ref={heading} tabIndex={-1} id="invitation-heading">
        Your invitation
      </h2>
      <p>Copy this message before closing. The code is shown only once.</p>
      <div className="invitation-preview">
        <h3>{title}</h3>
        <p>{instruction}</p>
        <dl>
          {fields.map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          ))}
        </dl>
      </div>
      <details open={editing} onToggle={(e) => setEditing(e.currentTarget.open)}>
        <summary>Shared server address</summary>
        <label>
          Address your guest can reach
          <input
            value={address}
            placeholder={addressText('friends.example', server.port)}
            onChange={(e) => {
              setAddress(e.target.value)
              setStatus('ready')
            }}
          />
        </label>
        {server.port !== DEFAULT_PORT && <p>Uses port {server.port} unless you enter another.</p>}
        <label>
          HTTPS port (if different)
          <input
            value={httpsPort}
            inputMode="numeric"
            onChange={(e) => {
              setHttpsPort(e.target.value)
              setStatus('ready')
            }}
          />
        </label>
        {problem && (
          <p>
            {address
              ? problem
              : 'This connection uses a local address. Enter the address your guest uses to reach this server.'}
          </p>
        )}
      </details>
      {status === 'manual' && (
        <label>
          Clipboard unavailable. Copy the selected invitation manually.
          <textarea
            readOnly
            ref={manual}
            value={invitation}
            onFocus={(e) => e.target.select()}
            rows={6}
          />
        </label>
      )}
      <div className="invitation-actions">
        <button className="button primary" disabled={!!problem} onClick={() => void copy()}>
          Copy invitation
        </button>
        <button className="button secondary" onClick={done}>
          Back to invites
        </button>
      </div>
      {status === 'copied' && <p role="status">Invitation copied.</p>}
    </section>
  )
}
