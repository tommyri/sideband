import { useEffect, useRef, useState } from 'react'
import { useShallow } from 'zustand/react/shallow'
import { useAppStore } from '../../store/app-store'
import type { AccountLinkReview } from '../../lib/generated/AccountLinkReview'
import type { ServerSummary } from '../../lib/bindings'
import { initials } from '../../lib/initials'
import { linkServerAccount, reviewServerAccount } from './accountLinkApi'
import './account-linking.css'

type Result = { review?: AccountLinkReview; error?: string }
type Candidate = {
  id: string
  label: string
  address: string
  userId: string
  username: string
  role: string
}

const statusText = {
  eligible: 'Ready to link',
  linked: 'Linked to your profile',
  serverSignInOnly: 'Server sign-in only',
  otherProfile: 'Linked to a different profile',
  unauthorizedDevice: 'This device is not authorized',
}

function address(server: ServerSummary) {
  const host = server.host.includes(':') ? `[${server.host}]` : server.host
  return server.port === 443 ? host : `${host}:${server.port}`
}

function message(error: unknown) {
  if (error && typeof error === 'object' && 'message' in error && typeof error.message === 'string')
    return error.message
  return 'Could not verify this account. Check the connection and try again.'
}

export function AccountLinking() {
  const servers = useAppStore((s) => s.servers)
  const connected = useAppStore(
    useShallow((s) => s.servers.filter((v) => v.status.kind === 'connected').map((v) => v.id)),
  )
  const reconnect = useAppStore((s) => s.setConnectOpen)
  const [results, setResults] = useState<Record<string, Result>>({})
  const [batch, setBatch] = useState<Candidate[] | null>(null)
  const [selected, setSelected] = useState<string[]>([])
  const [busy, setBusy] = useState(false)
  const [notice, setNotice] = useState('')
  const [refresh, setRefresh] = useState(0)
  const mounted = useRef(true)
  const working = useRef(false)
  const reads = useRef(new Map<string, number>())
  const heading = useRef<HTMLHeadingElement>(null)
  const returnTo = useRef<HTMLElement | null>(null)

  useEffect(() => {
    mounted.current = true
    return () => {
      mounted.current = false
    }
  }, [])

  useEffect(() => {
    let cancelled = false
    for (const id of connected) {
      const generation = (reads.current.get(id) ?? 0) + 1
      reads.current.set(id, generation)
      const current = () => !cancelled && reads.current.get(id) === generation
      void reviewServerAccount(id).then(
        (review) => {
          if (current()) setResults((prev) => ({ ...prev, [id]: { review } }))
        },
        (error: unknown) => {
          if (current()) setResults((prev) => ({ ...prev, [id]: { error: message(error) } }))
        },
      )
    }
    return () => {
      cancelled = true
    }
  }, [connected, refresh])

  useEffect(() => {
    if (batch) heading.current?.focus()
    else if (returnTo.current) {
      if (returnTo.current.isConnected) returnTo.current.focus()
      else heading.current?.focus()
      returnTo.current = null
    }
  }, [batch])

  function candidate(server: ServerSummary): Candidate | null {
    const review = results[server.id]?.review
    if (
      server.status.kind !== 'connected' ||
      review?.status !== 'eligible' ||
      !review.userId ||
      !review.role ||
      !review.username
    )
      return null
    return {
      id: server.id,
      label: server.label,
      address: address(server),
      userId: review.userId,
      username: review.username,
      role: review.role,
    }
  }

  function review(accounts: Candidate[]) {
    returnTo.current = document.activeElement as HTMLElement | null
    setNotice('')
    setSelected(accounts.map((account) => account.id))
    setBatch(accounts)
  }

  async function confirm() {
    if (working.current || !batch) return
    working.current = true
    setBusy(true)
    let done = 0
    let failed = 0
    for (const account of batch.filter((entry) => selected.includes(entry.id))) {
      if (!mounted.current) break
      reads.current.set(account.id, (reads.current.get(account.id) ?? 0) + 1)
      try {
        await linkServerAccount(account.id, account.userId, account.role)
        done++
        if (mounted.current)
          setResults((prev) => ({
            ...prev,
            [account.id]: {
              review: {
                status: 'linked',
                username: account.username,
                role: account.role,
                userId: account.userId,
              },
            },
          }))
      } catch (error) {
        failed++
        if (mounted.current)
          setResults((prev) => ({ ...prev, [account.id]: { error: message(error) } }))
      }
    }
    working.current = false
    if (mounted.current) {
      setBusy(false)
      setBatch(null)
      const success = done === 1 ? '1 account linked.' : `${done} accounts linked.`
      setNotice(failed ? `${success} Check the accounts below and retry.` : success)
    }
  }

  const eligible = servers.map(candidate).filter((account) => account !== null)
  const remaining = servers.some((s) => results[s.id]?.review?.status === 'linked')

  if (batch) {
    const single = batch.length === 1 ? batch[0] : undefined
    return (
      <section className="account-link" aria-labelledby="account-link-title">
        <h3 tabIndex={-1} ref={heading} id="account-link-title">
          {single ? `Link your ${single.label} account?` : 'Review accounts to link'}
        </h3>
        <p>
          {single
            ? 'This server recognizes the account already signed in on this computer.'
            : 'Choose the signed-in accounts you want to add to your profile.'}
        </p>
        <ul>
          {batch.map((account) => (
            <li key={account.id}>
              {single ? (
                <span className="account-mark" aria-hidden="true">
                  {initials(account.label)}
                </span>
              ) : (
                <input
                  type="checkbox"
                  aria-label={`Link ${account.label}`}
                  disabled={busy}
                  checked={selected.includes(account.id)}
                  onChange={(e) =>
                    setSelected(
                      e.target.checked
                        ? [...selected, account.id]
                        : selected.filter((id) => id !== account.id),
                    )
                  }
                />
              )}
              <div>
                <strong>{account.label}</strong>
                <span>
                  {account.address} · Signed in as {account.username}
                </span>
                <span>Role: {account.role}</span>
              </div>
            </li>
          ))}
        </ul>
        <p>Your membership, roles and channel access stay with each account you link.</p>
        <div className="account-link-actions">
          <button
            className="button primary"
            disabled={busy || !selected.length}
            onClick={() => void confirm()}
          >
            {busy ? 'Linking…' : single ? 'Link this account' : `Link ${selected.length} accounts`}
          </button>
          <button className="button secondary" disabled={busy} onClick={() => setBatch(null)}>
            Cancel
          </button>
        </div>
      </section>
    )
  }

  return (
    <section className="account-link" aria-labelledby="account-link-title">
      <h3 tabIndex={-1} ref={heading} id="account-link-title">
        Link existing accounts
      </h3>
      <p>
        {servers.length
          ? 'Choose an account you already use on this computer.'
          : 'Join a server with your existing account to link it here.'}
      </p>
      {notice && <p role="status">{notice}</p>}
      <ul>
        {servers.map((server) => {
          const result = results[server.id]
          const account = candidate(server)
          const online = server.status.kind === 'connected'
          const label = !online
            ? 'Sign in to check this account'
            : result?.error || (result?.review ? statusText[result.review.status] : 'Checking…')
          return (
            <li key={server.id}>
              <span className="account-mark" aria-hidden="true">
                {initials(server.label)}
              </span>
              <div>
                <strong>{server.label}</strong>
                <span>{address(server)}</span>
                <span>{label}</span>
              </div>
              {account && (
                <button
                  className="button secondary"
                  onClick={() => review([account])}
                  aria-label={`Review ${server.label}`}
                >
                  Review
                </button>
              )}
              {!online && (
                <button className="button secondary" onClick={() => reconnect(true, server)}>
                  Sign in
                </button>
              )}
              {online && result?.error && (
                <button
                  className="button secondary"
                  onClick={() => setRefresh((v) => v + 1)}
                  aria-label={`Check ${server.label} again`}
                >
                  Check again
                </button>
              )}
            </li>
          )
        })}
      </ul>
      {eligible.length > 1 && (
        <button className="button secondary" onClick={() => review(eligible)}>
          Review {eligible.length} {remaining ? 'other accounts' : 'accounts together'}
        </button>
      )}
      {servers.some((s) => results[s.id]?.review?.status === 'serverSignInOnly') && (
        <p>For servers marked “Server sign-in only,” keep using your usual login on each device.</p>
      )}
    </section>
  )
}
